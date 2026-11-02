// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Model-role settings commands for Mnemo2's Settings dialog — the
//! backend of its **Models** and **Savings** pages (plan d6a64d60).
//!
//! Mnemo2 shares Mnemo1's config dir, so every value here edits the same
//! `config.toml`/`endpoints.toml` the engine reads:
//!
//! - **Models page** — the per-task `[models]` slots Mnemo2's page owns
//!   (`planning`, `executing`, `bug_fixing`, `reviewing`, `summarize`),
//!   `general.vision_model`, and the code-indexing (bundled embedding)
//!   model with its catalog, install surface and status.
//! - **Savings page** — the `[general.optimizer]` levers + knobs, the
//!   per-model `[[pricing]]` table, and the `[general.budget]` caps.
//!
//! No config logic is forked: reads go through [`mnemo::config::Config`]
//! and the shared embedding catalog; every write rides the one
//! sanctioned pipeline — `Setup::patch_settings` →
//! `validate_and_apply_settings_patch` → `Config::save_all` — the same
//! one Mnemo1's `save_settings` runs, so the same validation, the same
//! on-disk result and the same live rewire apply. A save rewrites
//! `config.toml` fully from the in-memory schema (comments and unknown
//! keys are dropped by design — the schema is the source of truth), but
//! keeps every schema key, including sections no page owns, and the
//! `[tiers]` table of `endpoints.toml` rides through untouched.
//!
//! **When a save takes effect** — `[models]`, `vision_model`,
//! `optimizer`, `budget` and `pricing` are read when the engine is
//! built, so they apply after the project is reopened (or the lead
//! restarted); on an OPEN lead the pipeline additionally re-applies
//! what it can live (vision client, embedder, model resolver — the
//! settings save's rewire). An embedding selection live-swaps the
//! embedder on an open lead; a fresh download goes live at the next
//! reopen — Mnemo2 starts the engine with `auto_install_embedder:
//! false`, so until then recall stays keyword-only for a
//! not-yet-installed model.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use mnemo::config::settings_dto::{
    BudgetConfigDto, ModelRefDto, ModelsConfigDto, PricingDto, SettingsSaveDto, VisionModelDto,
    deserialize_optional_nullable,
};
use mnemo::config::{Config, ModelRef, EMBEDDING_MODEL_SENTINEL_HASH};
use mnemo::headless::{EventReceiver, RecvError, Setup, SetupEvent};
use mnemo::memory::embedder::EmbedderStatus;

use crate::mnemo2::ipc::{settings_refresh, settings_setup, Mnemo2State};

/// The Tauri event channel `embedding_install` streams its download
/// progress on: a [`SetupEvent`] payload tagged `SetupStep::EmbedderModel`
/// — `Progress` events with fractions while the download runs, then
/// `Done` or `Failed`. Named like Mnemo2's other channels
/// (`mnemo2://lead-event`, `mnemo2://team-event`).
pub const EMBEDDER_EVENT_CHANNEL: &str = "mnemo2://embedder-event";
// ── Wire shapes (the `model_settings_get` snapshot) ─────────────────────────

/// The full `model_settings_get` response — one snapshot of every
/// model-role section the Models and Savings pages edit.
#[derive(Debug, Clone, Serialize)]
pub struct ModelSettingsWire {
    /// Per-task `[models]` overrides.
    pub task_models: TaskModelsWire,
    /// The vision model as `"endpoint/model"`, or `null` (a multimodal
    /// main model serves vision directly).
    pub vision_model: Option<String>,
    /// The code-indexing (bundled embedding) model state.
    pub embedding: EmbeddingWire,
    /// The token-optimizer levers + knobs (`[general.optimizer]`).
    pub optimizer: OptimizerWire,
    /// The per-model pricing table (`[[pricing]]`, `$` per 1M tokens).
    pub pricing: Vec<PricingWire>,
    /// The deterministic budget caps (`[general.budget]`).
    pub budget: BudgetWire,
}

/// The five per-task `[models]` slots Mnemo2's Models page owns, each a
/// compact `"endpoint/model"` string or `null` (no override — the
/// tier/lead model applies).
///
/// Slots outside this page (`complete`, `free`, `subagent`, per-skill)
/// are neither read nor written here and survive every save; a slot
/// carrying a `reasoning_effort` override keeps it when a save names the
/// same endpoint + model (a changed model drops the effort — it belonged
/// to the old pairing).
#[derive(Debug, Clone, Serialize)]
pub struct TaskModelsWire {
    /// The planning model override.
    pub planning: Option<String>,
    /// The executing (step) model override.
    pub executing: Option<String>,
    /// The bug-fixing lane's model override.
    pub bug_fixing: Option<String>,
    /// The reviewing lane's model override.
    pub reviewing: Option<String>,
    /// The summarize (compaction) model override.
    pub summarize: Option<String>,
}

/// The code-indexing model section: what is selected, what can replace
/// it, and where the embedder stands.
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingWire {
    /// The effective bundled model id — the configured one, or the
    /// out-of-box default (`all-MiniLM-L6-v2`) when none is. The
    /// `"hash"` sentinel means explicit keyword-only.
    pub selected: String,
    /// The curated catalog (each flagged `installed`), plus the
    /// `"hash"` (Keyword only) entry.
    pub options: Vec<EmbeddingOptionWire>,
    /// One of `"ready"` (installed — semantic recall from the next
    /// reopen), `"missing"` (not installed — Install to use it),
    /// `"downloading"` (an install is in progress), `"failed"` (the
    /// last download or load failed), `"keyword_only"` (the hash
    /// sentinel is selected).
    pub status: &'static str,
}

/// One selectable code-indexing model.
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingOptionWire {
    /// The config value (`general.bundled_embedding_model`).
    pub id: String,
    /// Human-friendly display name (the `"hash"` sentinel shows as
    /// "Keyword only").
    pub label: String,
    /// Approximate download size in MB (`0` for the built-in hash
    /// embedder).
    pub size_mb: usize,
    /// Installed in this config dir's cache (`<config>/models`)?
    pub installed: bool,
}

/// The token-optimizer levers (bools, all on by default) + knobs —
/// `[general.optimizer]`, the Savings page's switches and numbers.
#[derive(Debug, Clone, Serialize)]
pub struct OptimizerWire {
    /// Lever: signature/diff re-reads instead of full file content.
    pub delta_reads: bool,
    /// Lever: semantic shell-output compression.
    pub compress_output: bool,
    /// Lever: archive/expand progressive disclosure of big results.
    pub archive: bool,
    /// Lever: compaction survival (checkpoint + decisions + digest).
    pub compaction_survival: bool,
    /// Lever: the S-F context-quality score rides usage events.
    pub quality_score: bool,
    /// Lever: lean-output nudge at high context fill.
    pub lean_output_nudge: bool,
    /// Lever: compact auto-recall re-injections of unchanged memories.
    pub recall_delta: bool,
    /// Knob: minimum tool-result length (chars) lever 3 archives.
    pub archive_min_chars: usize,
    /// Knob: minimum filtered-output length (chars) lever 2 compresses.
    pub compress_min_chars: usize,
    /// Knob: context fill % that triggers the lean-output nudge.
    pub lean_output_fill_pct: u8,
    /// Knob: requests between nudges (cooldown).
    pub nudge_cooldown_requests: u32,
    /// Knob: user-added command patterns eligible for compression.
    pub compress_extra_commands: Vec<String>,
}

/// One per-model pricing row (`[[pricing]]`, `$` per 1M tokens) — the
/// numbers the Cost tab's dollar figures are computed from.
#[derive(Debug, Clone, Serialize)]
pub struct PricingWire {
    /// The model id this pricing applies to.
    pub model: String,
    /// `$` per 1M input (prompt) tokens.
    pub input_per_1m: f64,
    /// `$` per 1M output (completion) tokens.
    pub output_per_1m: f64,
    /// `$` per 1M cached prompt tokens (the discounted cache rate).
    pub cached_per_1m: f64,
}

/// The deterministic budget caps (`[general.budget]`). Every field is
/// reported explicitly (the config section omits default values on
/// disk; the page needs the full picture).
#[derive(Debug, Clone, Serialize)]
pub struct BudgetWire {
    /// Whether the caps are enforced at all.
    pub enabled: bool,
    /// Total prompt+completion tokens a single plan may spend (`0` =
    /// no token cap).
    pub max_tokens_per_plan: u64,
    /// Escalations a single plan may arm before the budget asks.
    pub max_escalations_per_plan: u32,
    /// Failed-cycle retries a single plan step may take.
    pub max_retries_per_lane: u32,
}// ── Read (the `model_settings_get` core) ───────────────────────────────────

/// Read every model-role section from the config dir this `Setup`
/// targets — the core of `model_settings_get`, testable without Tauri
/// state. The embedding catalog is checked against the same dir, and
/// `status` reflects the live embedder when a lead is open.
pub(crate) fn model_settings_read(setup: &Setup) -> Result<ModelSettingsWire, String> {
    let config = Config::load(setup.config_dir()).map_err(|err| err.to_string())?;
    let general = &config.general.general;
    Ok(ModelSettingsWire {
        task_models: TaskModelsWire {
            planning: compact_model_ref(config.general.models.planning.as_ref()),
            executing: compact_model_ref(config.general.models.executing.as_ref()),
            bug_fixing: compact_model_ref(config.general.models.bug_fixing.as_ref()),
            reviewing: compact_model_ref(config.general.models.reviewing.as_ref()),
            summarize: compact_model_ref(config.general.models.summarize.as_ref()),
        },
        vision_model: general
            .vision_model
            .as_ref()
            .map(|v| format!("{}/{}", v.endpoint, v.model)),
        embedding: embedding_wire(setup, &config),
        optimizer: OptimizerWire {
            delta_reads: general.optimizer.delta_reads,
            compress_output: general.optimizer.compress_output,
            archive: general.optimizer.archive,
            compaction_survival: general.optimizer.compaction_survival,
            quality_score: general.optimizer.quality_score,
            lean_output_nudge: general.optimizer.lean_output_nudge,
            recall_delta: general.optimizer.recall_delta,
            archive_min_chars: general.optimizer.archive_min_chars,
            compress_min_chars: general.optimizer.compress_min_chars,
            lean_output_fill_pct: general.optimizer.lean_output_fill_pct,
            nudge_cooldown_requests: general.optimizer.nudge_cooldown_requests,
            compress_extra_commands: general.optimizer.compress_extra_commands.clone(),
        },
        pricing: config
            .pricing
            .iter()
            .map(|p| PricingWire {
                model: p.model.clone(),
                input_per_1m: p.input_per_1m,
                output_per_1m: p.output_per_1m,
                cached_per_1m: p.cached_per_1m,
            })
            .collect(),
        budget: BudgetWire {
            enabled: general.budget.enabled,
            max_tokens_per_plan: general.budget.max_tokens_per_plan,
            max_escalations_per_plan: general.budget.max_escalations_per_plan,
            max_retries_per_lane: general.budget.max_retries_per_lane,
        },
    })
}

/// Compact wire form of one `[models]` slot: `"endpoint/model"`.
fn compact_model_ref(r: Option<&ModelRef>) -> Option<String> {
    r.map(|r| format!("{}/{}", r.endpoint, r.model))
}

/// Build the code-indexing section: the curated catalog flagged against
/// THIS config dir's cache, the effective selection, and the status.
fn embedding_wire(setup: &Setup, config: &Config) -> EmbeddingWire {
    let catalog = setup.embedding_models();
    let selected = config
        .general
        .general
        .bundled_embedding_model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| {
            mnemo::config::general::default_bundled_embedding_model()
                .unwrap_or_else(|| EMBEDDING_MODEL_SENTINEL_HASH.to_string())
        });
    let installed = catalog
        .iter()
        .any(|m| m.installed && m.id.eq_ignore_ascii_case(&selected));
    let status = if selected.eq_ignore_ascii_case(EMBEDDING_MODEL_SENTINEL_HASH) {
        "keyword_only"
    } else {
        match setup.embedder_status() {
            EmbedderStatus::Downloading { .. } => "downloading",
            // Fallback = the embedder degraded to hash mode after a
            // failure; the chosen model is not serving.
            EmbedderStatus::Failed | EmbedderStatus::Fallback => "failed",
            _ if installed => "ready",
            _ => "missing",
        }
    };
    let mut options: Vec<EmbeddingOptionWire> = catalog
        .into_iter()
        .map(|m| EmbeddingOptionWire {
            id: m.id,
            label: m.name,
            size_mb: m.size_mb,
            installed: m.installed,
        })
        .collect();
    options.push(EmbeddingOptionWire {
        id: EMBEDDING_MODEL_SENTINEL_HASH.to_string(),
        label: "Keyword only".to_string(),
        size_mb: 0,
        installed: true,
    });
    EmbeddingWire {
        selected,
        options,
        status,
    }
}// ── Save (the `model_settings_save` core) ──────────────────────────────────

/// Apply one page-owned section. `section` is one of `"models"`,
/// `"vision"`, `"embedding"`, `"optimizer"`, `"pricing"` or `"budget"`
/// (anything else rejects); `value` is that section's payload. Every
/// write rides `Setup::patch_settings` — the shared validate → persist
/// → live-rewire pipeline — so only the named section changes.
pub(crate) async fn model_settings_apply(
    setup: &Setup,
    section: &str,
    value: Value,
) -> Result<(), String> {
    match section {
        "models" => apply_models(setup, value).await,
        "vision" => apply_vision(setup, value).await,
        "embedding" => apply_embedding(setup, value).await,
        "optimizer" => apply_optimizer(setup, value).await,
        "pricing" => apply_pricing(setup, value).await,
        "budget" => apply_budget(setup, value).await,
        other => Err(format!(
            "unknown model-settings section '{other}' \
             (expected models, vision, embedding, optimizer, pricing or budget)"
        )),
    }
}

/// Split a compact `"endpoint/model"` picker value at the FIRST slash:
/// endpoint names cannot contain a slash, model ids like
/// `org/model-name` survive intact.
fn parse_model_ref(text: &str) -> Result<(String, String), String> {
    let text = text.trim();
    let invalid = || format!("model must be 'endpoint/model' (got '{text}')");
    let Some((endpoint, model)) = text.split_once('/') else {
        return Err(invalid());
    };
    let (endpoint, model) = (endpoint.trim(), model.trim());
    if endpoint.is_empty() || model.is_empty() {
        return Err(invalid());
    }
    Ok((endpoint.to_string(), model.to_string()))
}

/// The `"models"` save payload: the five task slots, with the shared
/// double-Option semantics — absent keeps, `null` clears, a string
/// sets.
#[derive(Default, Deserialize)]
#[serde(default)]
struct TaskModelsPatch {
    #[serde(deserialize_with = "deserialize_optional_nullable")]
    planning: Option<Option<String>>,
    #[serde(deserialize_with = "deserialize_optional_nullable")]
    executing: Option<Option<String>>,
    #[serde(deserialize_with = "deserialize_optional_nullable")]
    bug_fixing: Option<Option<String>>,
    #[serde(deserialize_with = "deserialize_optional_nullable")]
    reviewing: Option<Option<String>>,
    #[serde(deserialize_with = "deserialize_optional_nullable")]
    summarize: Option<Option<String>>,
}

/// Convert one slot patch to its DTO form, preserving the stored
/// `reasoning_effort` when the endpoint + model are unchanged.
fn slot_dto(
    patch: Option<Option<String>>,
    current: Option<&ModelRef>,
) -> Result<Option<Option<ModelRefDto>>, String> {
    match patch {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(text)) => {
            let (endpoint, model) = parse_model_ref(&text)?;
            let reasoning_effort = current
                .filter(|r| r.endpoint == endpoint && r.model == model)
                .and_then(|r| r.reasoning_effort.clone());
            Ok(Some(Some(ModelRefDto {
                endpoint,
                model,
                reasoning_effort,
            })))
        }
    }
}

/// The `"models"` section: `{planning?, executing?, bug_fixing?,
/// reviewing?, summarize?}`, each `"endpoint/model"` (set), `null`
/// (clear) or absent (keep). Slots outside the page are untouched.
async fn apply_models(setup: &Setup, value: Value) -> Result<(), String> {
    let patch: TaskModelsPatch = serde_json::from_value(value)
        .map_err(|err| format!("invalid models section: {err}"))?;
    let current = Config::load(setup.config_dir()).map_err(|err| err.to_string())?;
    let slots = &current.general.models;
    let dto = SettingsSaveDto {
        models: Some(ModelsConfigDto {
            planning: slot_dto(patch.planning, slots.planning.as_ref())?,
            executing: slot_dto(patch.executing, slots.executing.as_ref())?,
            bug_fixing: slot_dto(patch.bug_fixing, slots.bug_fixing.as_ref())?,
            reviewing: slot_dto(patch.reviewing, slots.reviewing.as_ref())?,
            summarize: slot_dto(patch.summarize, slots.summarize.as_ref())?,
            ..Default::default()
        }),
        ..Default::default()
    };
    setup
        .patch_settings(dto)
        .await
        .map_err(|err| err.to_string())
}

/// The `"vision"` section: `"endpoint/model"` sets, `null` clears.
async fn apply_vision(setup: &Setup, value: Value) -> Result<(), String> {
    let dto = match value {
        Value::Null => SettingsSaveDto {
            clear_vision_model: true,
            ..Default::default()
        },
        Value::String(text) => {
            let (endpoint, model) = parse_model_ref(&text)?;
            SettingsSaveDto {
                vision_model: Some(VisionModelDto { endpoint, model }),
                ..Default::default()
            }
        }
        _ => return Err("vision_model must be 'endpoint/model' or null".into()),
    };
    setup
        .patch_settings(dto)
        .await
        .map_err(|err| err.to_string())
}

/// The `"embedding"` section: a known bundled model id (or `"hash"`)
/// selects, riding `Setup::select_embedding_model`'s known-id guard +
/// full pipeline; `null` is the same opt-out as `"hash"` (the shared
/// patch's clear leg writes the sentinel).
async fn apply_embedding(setup: &Setup, value: Value) -> Result<(), String> {
    match value {
        Value::Null => {
            let dto = SettingsSaveDto {
                clear_bundled_embedding_model: true,
                ..Default::default()
            };
            setup
                .patch_settings(dto)
                .await
                .map_err(|err| err.to_string())
        }
        Value::String(id) => setup
            .select_embedding_model(id.trim())
            .await
            .map_err(|err| err.to_string()),
        _ => Err("embedding model must be a model id, 'hash' or null".into()),
    }
}

/// The `"optimizer"` save payload — mirrors [`OptimizerWire`]'s field
/// names; every field optional (absent = keep).
#[derive(Default, Deserialize)]
#[serde(default)]
struct OptimizerPatch {
    delta_reads: Option<bool>,
    compress_output: Option<bool>,
    archive: Option<bool>,
    compaction_survival: Option<bool>,
    quality_score: Option<bool>,
    lean_output_nudge: Option<bool>,
    recall_delta: Option<bool>,
    archive_min_chars: Option<usize>,
    compress_min_chars: Option<usize>,
    lean_output_fill_pct: Option<u8>,
    nudge_cooldown_requests: Option<u32>,
    compress_extra_commands: Option<Vec<String>>,
}

/// The `"optimizer"` section: a partial lever/knob object; absent fields
/// keep.
async fn apply_optimizer(setup: &Setup, value: Value) -> Result<(), String> {
    let patch: OptimizerPatch = serde_json::from_value(value)
        .map_err(|err| format!("invalid optimizer section: {err}"))?;
    let dto = SettingsSaveDto {
        optimizer_delta_reads: patch.delta_reads,
        optimizer_compress_output: patch.compress_output,
        optimizer_archive: patch.archive,
        optimizer_compaction_survival: patch.compaction_survival,
        optimizer_quality_score: patch.quality_score,
        optimizer_lean_output_nudge: patch.lean_output_nudge,
        optimizer_recall_delta: patch.recall_delta,
        optimizer_archive_min_chars: patch.archive_min_chars,
        optimizer_compress_min_chars: patch.compress_min_chars,
        optimizer_lean_output_fill_pct: patch.lean_output_fill_pct,
        optimizer_nudge_cooldown_requests: patch.nudge_cooldown_requests,
        optimizer_compress_extra_commands: patch.compress_extra_commands,
        ..Default::default()
    };
    setup
        .patch_settings(dto)
        .await
        .map_err(|err| err.to_string())
}

/// The `"pricing"` section: the full `[[pricing]]` array (a save
/// replaces the table; an empty array clears it).
async fn apply_pricing(setup: &Setup, value: Value) -> Result<(), String> {
    let rows: Vec<PricingDto> = serde_json::from_value(value)
        .map_err(|err| format!("invalid pricing section: {err}"))?;
    let dto = SettingsSaveDto {
        pricing: Some(rows),
        ..Default::default()
    };
    setup
        .patch_settings(dto)
        .await
        .map_err(|err| err.to_string())
}

/// The `"budget"` section: a partial caps object; absent fields keep.
async fn apply_budget(setup: &Setup, value: Value) -> Result<(), String> {
    let caps: BudgetConfigDto = serde_json::from_value(value)
        .map_err(|err| format!("invalid budget section: {err}"))?;
    let dto = SettingsSaveDto {
        budget: Some(caps),
        ..Default::default()
    };
    setup
        .patch_settings(dto)
        .await
        .map_err(|err| err.to_string())
}// ── Tauri commands ─────────────────────────────────────────────────────────

/// Read every model-role section for the Models and Savings pages.
/// With a lead open this targets the lead's config dir (and reports the
/// live embedder status); otherwise the machine-global one.
#[tauri::command]
pub async fn model_settings_get(
    state: State<'_, Mnemo2State>,
) -> Result<ModelSettingsWire, String> {
    let (setup, _) = settings_setup(&state).await?;
    model_settings_read(&setup)
}

/// Save one page-owned section (`"models"`, `"vision"`, `"embedding"`,
/// `"optimizer"`, `"pricing"` or `"budget"`), writing only that section
/// and keeping every other key. The engine reads `[models]`,
/// `vision_model`, `optimizer`, `budget` and `pricing` at build time, so
/// those apply after the project is reopened; an open lead additionally
/// gets the pipeline's live rewire, and the lead/team catalog is
/// refreshed so the next dispatch sees the new models and prices.
#[tauri::command]
pub async fn model_settings_save(
    state: State<'_, Mnemo2State>,
    section: String,
    value: Value,
) -> Result<(), String> {
    let (setup, runtime) = settings_setup(&state).await?;
    model_settings_apply(&setup, &section, value).await?;
    settings_refresh(runtime).await
}

/// Download a bundled code-indexing model in the background, streaming
/// [`SetupEvent`] progress (fractions, then `Done`/`Failed`) on
/// [`EMBEDDER_EVENT_CHANNEL`]. Fails fast on an unknown id. The download
/// lands in this config dir's cache; the model goes live at the next
/// settings save or reopen.
#[tauri::command]
pub async fn embedding_install(
    state: State<'_, Mnemo2State>,
    app: AppHandle,
    model_id: String,
) -> Result<(), String> {
    let (setup, _) = settings_setup(&state).await?;
    let events = setup
        .install_embedding_model(model_id.trim())
        .map_err(|err| err.to_string())?;
    spawn_embedder_forwarder(app, events);
    Ok(())
}

/// Forward the install's [`SetupEvent`]s to the webview until the stream
/// closes, tolerating broadcast lag (the frontend re-fetches status on
/// the terminal event).
fn spawn_embedder_forwarder(app: AppHandle, mut events: EventReceiver<SetupEvent>) {
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = app.emit(EMBEDDER_EVENT_CHANNEL, &event);
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
}// ── Tests ──────────────────────────────────────────────────────────────────
//
// The round-trip tests run against a fixture config with sections the
// pages do NOT own (safety, [context], [ui], [markdown], the foreign
// [models] slots, [tiers]) and assert those survive every save — the
// shared pipeline rewrites config.toml fully from the schema, so
// surviving keys prove only the named section was patched. Comments
// and unknown keys are dropped by that same documented design (the
// schema is the source of truth — identical to Mnemo1's save_settings).

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::config::general::SafetyMode;
    use serde_json::json;

    /// A temp config dir with a rich fixture: a `config.toml` whose
    /// page-owned sections sit beside ones no page touches, and an
    /// `endpoints.toml` with an endpoint, a pricing row and tiers.
    /// Returns the TempDir (kept alive) and a `Setup` on it.
    fn fixture() -> (tempfile::TempDir, Setup) {
        let dir = tempfile::tempdir().expect("config dir");
        std::fs::write(
            dir.path().join("config.toml"),
            r#"[general]
safety = "auto-approve-project"

[general.vision_model]
endpoint = "ep1"
model = "vision-x"

[general.optimizer]
archive = false
archive_min_chars = 1234

[general.budget]
enabled = true
max_tokens_per_plan = 5000

[models]
planning = { endpoint = "ep1", model = "plan-model", reasoning_effort = "high" }
executing = { endpoint = "ep1", model = "exec-model" }
complete = { endpoint = "ep1", model = "complete-model" }
subagent = { endpoint = "ep1", model = "sub-model" }

[context]
summarize_at_fill_rate = 0.5

[ui]
theme = "dark"

[markdown]
skip_dirs = ["target"]
"#,
        )
        .expect("write config.toml");
        std::fs::write(
            dir.path().join("endpoints.toml"),
            r#"[[endpoint]]
name = "ep1"
kind = "openai"
base_url = "http://127.0.0.1:9/v1"
models = ["plan-model", "exec-model", "complete-model", "sub-model", "vision-x", "other-model"]

[[pricing]]
model = "plan-model"
input_per_1m = 1.5
output_per_1m = 2.5
cached_per_1m = 0.1

[tiers]
difficult = ["ep1/plan-model"]
average = ["ep1/exec-model"]
simple = ["ep1/exec-model"]
"#,
        )
        .expect("write endpoints.toml");
        let setup = Setup::open(Some(dir.path().to_path_buf())).expect("setup");
        (dir, setup)
    }

    #[test]
    fn get_reads_every_section_from_the_fixture() {
        let (_dir, setup) = fixture();
        let wire = model_settings_read(&setup).expect("read");
        assert_eq!(wire.task_models.planning.as_deref(), Some("ep1/plan-model"));
        assert_eq!(wire.task_models.executing.as_deref(), Some("ep1/exec-model"));
        assert_eq!(wire.task_models.bug_fixing, None);
        assert_eq!(wire.task_models.reviewing, None);
        assert_eq!(wire.task_models.summarize, None);
        assert_eq!(wire.vision_model.as_deref(), Some("ep1/vision-x"));
        assert!(!wire.optimizer.archive, "the fixture opts lever 3 out");
        assert_eq!(wire.optimizer.archive_min_chars, 1234);
        assert!(wire.optimizer.delta_reads, "untouched levers stay default");
        assert_eq!(wire.pricing.len(), 1);
        assert_eq!(wire.pricing[0].model, "plan-model");
        assert_eq!(wire.pricing[0].input_per_1m, 1.5);
        assert_eq!(wire.pricing[0].output_per_1m, 2.5);
        assert_eq!(wire.pricing[0].cached_per_1m, 0.1);
        assert!(wire.budget.enabled);
        assert_eq!(wire.budget.max_tokens_per_plan, 5000);
        assert_eq!(wire.budget.max_escalations_per_plan, 1, "the default fills in");
        assert_eq!(wire.budget.max_retries_per_lane, 3, "the default fills in");
        // Nothing is configured, so the out-of-box default resolves; it
        // is not installed in the temp dir, and the hash sentinel rides
        // along as an option.
        assert_eq!(wire.embedding.selected, "all-MiniLM-L6-v2");
        assert_eq!(wire.embedding.status, "missing");
        assert!(wire
            .embedding
            .options
            .iter()
            .any(|o| o.id == "hash" && o.label == "Keyword only"));
        assert!(wire
            .embedding
            .options
            .iter()
            .any(|o| o.id == "all-MiniLM-L6-v2" && !o.installed));
    }

    #[test]
    fn an_installed_model_reports_ready() {
        let (dir, setup) = fixture();
        let cache = dir
            .path()
            .join("models")
            .join("models--Qdrant--all-MiniLM-L6-v2-onnx")
            .join("snapshots")
            .join("rev1");
        std::fs::create_dir_all(&cache).expect("create cache");
        std::fs::write(cache.join("model.onnx"), b"fake onnx").expect("write onnx");
        let wire = model_settings_read(&setup).expect("read");
        assert!(wire
            .embedding
            .options
            .iter()
            .any(|o| o.id == "all-MiniLM-L6-v2" && o.installed));
        assert_eq!(wire.embedding.status, "ready");
    }

    #[tokio::test]
    async fn save_models_sets_clears_and_keeps_everything_else() {
        let (dir, setup) = fixture();
        model_settings_apply(
            &setup,
            "models",
            json!({ "planning": null, "bug_fixing": "ep1/other-model" }),
        )
        .await
        .expect("save");
        // The wire reflects the patch.
        let wire = model_settings_read(&setup).expect("read");
        assert_eq!(wire.task_models.planning, None, "null clears the slot");
        assert_eq!(wire.task_models.bug_fixing.as_deref(), Some("ep1/other-model"));
        assert_eq!(wire.task_models.executing.as_deref(), Some("ep1/exec-model"), "absent keeps");
        // Foreign [models] slots survive untouched.
        let config = Config::load(dir.path()).expect("reload");
        assert_eq!(config.general.models.complete.as_ref().map(|r| r.model.as_str()), Some("complete-model"));
        assert_eq!(config.general.models.subagent.as_ref().map(|r| r.model.as_str()), Some("sub-model"));
        // Unrelated schema keys survive the rewrite.
        assert_eq!(config.general.general.safety, SafetyMode::AutoApproveProject);
        assert_eq!(config.general.context.summarize_at_fill_rate, 0.5);
        assert_eq!(config.general.ui.theme, "dark");
        assert_eq!(config.general.markdown.skip_dirs, vec!["target".to_string()]);
        // And so does the [tiers] table save_all carries through.
        let tiers = mnemo::config::endpoints::load_tiers(&dir.path().join("endpoints.toml"));
        assert_eq!(tiers.difficult, vec!["ep1/plan-model".to_string()]);
    }

    #[tokio::test]
    async fn save_models_preserves_reasoning_effort_and_validates() {
        let (dir, setup) = fixture();
        // An identity match keeps the stored reasoning effort.
        model_settings_apply(&setup, "models", json!({ "planning": "ep1/plan-model" }))
            .await
            .expect("save");
        let config = Config::load(dir.path()).expect("reload");
        assert_eq!(
            config.general.models.planning.as_ref().unwrap().reasoning_effort.as_deref(),
            Some("high")
        );
        // A changed model drops it — the effort belonged to the old pair.
        model_settings_apply(&setup, "models", json!({ "planning": "ep1/other-model" }))
            .await
            .expect("save");
        let config = Config::load(dir.path()).expect("reload");
        assert_eq!(config.general.models.planning.as_ref().unwrap().reasoning_effort, None);
        // The shared pipeline's cross-reference validation rejects an
        // unknown endpoint, and the compact form must be "endpoint/model".
        let err = model_settings_apply(&setup, "models", json!({ "reviewing": "ep9/x" }))
            .await
            .expect_err("unknown endpoint");
        assert!(err.contains("does not match any configured endpoint"), "{err}");
        let err = model_settings_apply(&setup, "models", json!({ "reviewing": "ep1" }))
            .await
            .expect_err("no slash");
        assert!(err.contains("endpoint/model"), "{err}");
    }    #[tokio::test]
    async fn save_vision_sets_clears_and_validates() {
        let (_dir, setup) = fixture();
        model_settings_apply(&setup, "vision", json!("ep1/other-model"))
            .await
            .expect("save");
        let wire = model_settings_read(&setup).expect("read");
        assert_eq!(wire.vision_model.as_deref(), Some("ep1/other-model"));
        model_settings_apply(&setup, "vision", Value::Null)
            .await
            .expect("clear");
        let wire = model_settings_read(&setup).expect("read");
        assert_eq!(wire.vision_model, None);
        let err = model_settings_apply(&setup, "vision", json!("ep9/x"))
            .await
            .expect_err("unknown endpoint");
        assert!(err.contains("does not match any configured endpoint"), "{err}");
        let err = model_settings_apply(&setup, "vision", json!(42))
            .await
            .expect_err("not a string");
        assert!(err.contains("vision_model must be"), "{err}");
    }

    #[tokio::test]
    async fn save_embedding_selects_known_ids_and_reports_status() {
        let (_dir, setup) = fixture();
        model_settings_apply(&setup, "embedding", json!("all-MiniLM-L6-v2"))
            .await
            .expect("save");
        let wire = model_settings_read(&setup).expect("read");
        assert_eq!(wire.embedding.selected, "all-MiniLM-L6-v2");
        assert_eq!(wire.embedding.status, "missing", "nothing is installed yet");
        // The hash sentinel is the explicit keyword-only opt-out…
        model_settings_apply(&setup, "embedding", json!("hash"))
            .await
            .expect("opt out");
        let wire = model_settings_read(&setup).expect("read");
        assert_eq!(wire.embedding.selected, "hash");
        assert_eq!(wire.embedding.status, "keyword_only");
        // …and null clears the selection the same way.
        model_settings_apply(&setup, "embedding", Value::Null)
            .await
            .expect("clear");
        let wire = model_settings_read(&setup).expect("read");
        assert_eq!(wire.embedding.selected, "hash");
        assert_eq!(wire.embedding.status, "keyword_only");
        // A typo never reaches config.toml — the known-id guard rejects.
        let err = model_settings_apply(&setup, "embedding", json!("no-such-model"))
            .await
            .expect_err("unknown id");
        assert!(err.contains("unknown embedding model"), "{err}");
    }

    #[tokio::test]
    async fn save_optimizer_updates_only_present_fields() {
        let (_dir, setup) = fixture();
        model_settings_apply(
            &setup,
            "optimizer",
            json!({ "compress_output": false, "nudge_cooldown_requests": 20 }),
        )
        .await
        .expect("save");
        let wire = model_settings_read(&setup).expect("read");
        assert!(!wire.optimizer.compress_output, "the patch flipped the lever");
        assert_eq!(wire.optimizer.nudge_cooldown_requests, 20);
        assert!(!wire.optimizer.archive, "the fixture value survives");
        assert_eq!(wire.optimizer.archive_min_chars, 1234);
        assert!(wire.optimizer.delta_reads, "untouched levers stay default");
    }

    #[tokio::test]
    async fn save_pricing_replaces_the_table_and_keeps_tiers() {
        let (dir, setup) = fixture();
        model_settings_apply(
            &setup,
            "pricing",
            json!([
                { "model": "exec-model", "input_per_1m": 0.3, "output_per_1m": 0.9, "cached_per_1m": 0.05 },
                { "model": "other-model", "input_per_1m": 2.0, "output_per_1m": 6.0, "cached_per_1m": 1.0 }
            ]),
        )
        .await
        .expect("save");
        let wire = model_settings_read(&setup).expect("read");
        assert_eq!(wire.pricing.len(), 2);
        assert_eq!(wire.pricing[0].model, "exec-model");
        assert_eq!(wire.pricing[0].input_per_1m, 0.3);
        assert_eq!(wire.pricing[1].model, "other-model");
        assert_eq!(wire.pricing[1].output_per_1m, 6.0);
        // The shared validation rejects negative rates…
        let err = model_settings_apply(
            &setup,
            "pricing",
            json!([{ "model": "m", "input_per_1m": -1.0, "output_per_1m": 1.0, "cached_per_1m": 1.0 }]),
        )
        .await
        .expect_err("negative rate");
        assert!(err.contains("non-negative rates"), "{err}");
        // …and the endpoints.toml rewrite carries [tiers] through.
        let tiers = mnemo::config::endpoints::load_tiers(&dir.path().join("endpoints.toml"));
        assert_eq!(tiers.average, vec!["ep1/exec-model".to_string()]);
        let config = Config::load(dir.path()).expect("reload");
        assert_eq!(config.endpoints.len(), 1, "the endpoint survives the rewrite");
    }

    #[tokio::test]
    async fn save_budget_updates_caps_and_keeps_the_flag() {
        let (_dir, setup) = fixture();
        model_settings_apply(&setup, "budget", json!({ "max_tokens_per_plan": 999 }))
            .await
            .expect("save");
        let wire = model_settings_read(&setup).expect("read");
        assert_eq!(wire.budget.max_tokens_per_plan, 999, "the patch set the cap");
        assert!(wire.budget.enabled, "the fixture flag survives");
    }

    #[tokio::test]
    async fn an_unknown_section_rejects() {
        let (_dir, setup) = fixture();
        let err = model_settings_apply(&setup, "nope", json!({}))
            .await
            .expect_err("unknown section");
        assert!(err.contains("unknown model-settings section 'nope'"), "{err}");
    }
}