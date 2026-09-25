// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Settings-patch DTO types + validation + config assembly (review M1b).
//!
//! These types are the wire format for the `save_settings` Tauri command —
//! plain serde structs with no Tauri dependencies. They live in the brain
//! (not the IPC adapter) so the validation + config-assembly logic is
//! unit-testable without Tauri state, and the console runtime can reuse the
//! same validation if it ever gains a settings-save path.
//!
//! The IPC layer (`src-tauri/src/ipc/settings.rs`) constructs these from the
//! frontend payload, calls [`validate_and_apply_settings_patch`], then
//! persists + reloads + re-syncs the live provider — the Tauri-specific parts
//! that belong in the adapter.

use serde::{Deserialize, Serialize};

use crate::config::{Config, ModelRef};

// ── DTO types (wire format for save_settings) ─────────────────────────────

/// Optional vision model payload. Mirrors [`EmbeddingModelDto`] but is a
/// distinct type so the two config surfaces cannot be confused.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionModelDto {
    /// Endpoint name hosting the vision model.
    pub endpoint: String,
    /// Model id.
    pub model: String,
}

/// Optional embedding model payload. Mirrors [`VisionModelDto`] but is a
/// distinct type so the two config surfaces cannot be confused.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingModelDto {
    /// Endpoint name hosting the embedding model.
    pub endpoint: String,
    /// Model id (e.g. `nomic-embed-text`, `text-embedding-3-small`).
    pub model: String,
}

/// A per-context model override payload (one entry of the `[models]` section).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRefDto {
    /// Endpoint name (matches an entry in `endpoints.toml`).
    pub endpoint: String,
    /// Model id at that endpoint.
    pub model: String,
    /// Optional per-context reasoning-effort override; absent/`None` keeps
    /// the model's own effort default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// The `[models]` section patch. When present, fully replaces the `[models]`
/// section. Each field is optional so the frontend can send only the overrides
/// it manages; `None` fields keep the existing value. To *clear* an override,
/// send `null` for that field.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelsConfigDto {
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub planning: Option<Option<ModelRefDto>>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub executing: Option<Option<ModelRefDto>>,
    /// Model + reasoning effort for the active plan's bug-fixing lifecycle
    /// (overrides `executing` while the active plan's kind is `bug_fixing`
    /// and the workflow is Executing/Reviewing).
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub bug_fixing: Option<Option<ModelRefDto>>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub reviewing: Option<Option<ModelRefDto>>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub complete: Option<Option<ModelRefDto>>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub subagent: Option<Option<ModelRefDto>>,
    /// Model for compaction summaries (auto-compaction + the run-all
    /// between-items compact). Unset = the turn's model.
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    pub summarize: Option<Option<ModelRefDto>>,
    /// When present, fully replaces the per-skill override map (send `{}` to
    /// clear all skill overrides).
    #[serde(default)]
    pub skill: Option<std::collections::HashMap<String, ModelRefDto>>,
}

/// Deserialize an `Option<Option<T>>` so that a field **absent** from the JSON
/// yields `None` (outer — "keep the existing value"), while an explicit `null`
/// yields `Some(None)` (inner — "clear the override"). A present object yields
/// `Some(Some(T))` ("set/replace"). This is the standard serde workaround for
/// distinguishing absent vs null on a doubly-wrapped optional (the
/// `serde_with::DoubleOption` pattern, inlined to avoid a dependency).
pub fn deserialize_optional_nullable<'de, T, D>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    struct OptionalNullableVisitor<T>(std::marker::PhantomData<T>);

    impl<'de, T> serde::de::Visitor<'de> for OptionalNullableVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Option<Option<T>>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("null or a model override object")
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Some(None))
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Some(None))
        }

        fn visit_some<D2: serde::Deserializer<'de>>(
            self,
            deserializer: D2,
        ) -> Result<Self::Value, D2::Error> {
            T::deserialize(deserializer).map(|v| Some(Some(v)))
        }
    }

    deserializer.deserialize_option(OptionalNullableVisitor(std::marker::PhantomData))
}

/// Pricing row for [`save_settings`](crate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingDto {
    /// Model id.
    pub model: String,
    /// $ per 1M input tokens.
    pub input_per_1m: f64,
    /// $ per 1M output tokens.
    pub output_per_1m: f64,
    /// $ per 1M cached prompt tokens.
    pub cached_per_1m: f64,
}

/// The `[memory]` section patch — every field optional (an omitted field keeps
/// the existing value).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemorySearchDto {
    /// Recency half-life (days) for the strength decay in recall ranking.
    #[serde(default)]
    pub decay_half_life_days: Option<f64>,
    /// Default per-query result cap (an explicit filter limit always wins).
    #[serde(default)]
    pub per_query_cap: Option<usize>,
    /// Max derived records of one record type per recall result.
    #[serde(default)]
    pub derived_per_class_cap: Option<usize>,
    /// Digest auto-truncation lengths (chars) for derived/captured digests.
    #[serde(default)]
    pub plan_budget: Option<usize>,
    /// See `plan_budget`.
    #[serde(default)]
    pub bug_budget: Option<usize>,
    /// See `plan_budget`.
    #[serde(default)]
    pub spec_budget: Option<usize>,
    /// See `plan_budget`.
    #[serde(default)]
    pub decision_budget: Option<usize>,
    /// See `plan_budget`.
    #[serde(default)]
    pub review_budget: Option<usize>,
}

/// The `[ui.steering_notes]` section patch — every field optional (an omitted
/// field keeps the existing override).
///
/// The kinds + their defaults live in
/// [`SteeringNotesCfg`](crate::config::general::SteeringNotesCfg) and
/// `UiConfig::effective_steering_note_visible`; `None` here means "leave the
/// override as it is", not "reset to the default".
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SteeringNotesDto {
    /// See `SteeringNotesCfg::auto_delegated`.
    #[serde(default)]
    pub auto_delegated: Option<bool>,
    /// See `SteeringNotesCfg::search_nudge`.
    #[serde(default)]
    pub search_nudge: Option<bool>,
    /// See `SteeringNotesCfg::shell_tip`.
    #[serde(default)]
    pub shell_tip: Option<bool>,
    /// See `SteeringNotesCfg::graph_miss`.
    #[serde(default)]
    pub graph_miss: Option<bool>,
    /// See `SteeringNotesCfg::recall_rider`.
    #[serde(default)]
    pub recall_rider: Option<bool>,
    /// See `SteeringNotesCfg::read_nudge`.
    #[serde(default)]
    pub read_nudge: Option<bool>,
    /// See `SteeringNotesCfg::literal_tip`.
    #[serde(default)]
    pub literal_tip: Option<bool>,
    /// See `SteeringNotesCfg::known_memory_hit`.
    #[serde(default)]
    pub known_memory_hit: Option<bool>,
    /// See `SteeringNotesCfg::consolidation_due`.
    #[serde(default)]
    pub consolidation_due: Option<bool>,
    /// See `SteeringNotesCfg::shell_redirect`.
    #[serde(default)]
    pub shell_redirect: Option<bool>,
    /// See `SteeringNotesCfg::edit_stale_read`.
    #[serde(default)]
    pub edit_stale_read: Option<bool>,
}

/// Patch payload for non-endpoint Settings sections.
///
/// All fields are optional — only present fields are applied. Endpoints and
/// API keys continue to use `save_endpoints`. After a successful write the
/// in-memory config is reloaded and, if `safety` changed, the runtime
/// `safety_mode` lock is updated to match.
///
/// Vision model: set `clear_vision_model: true` to remove it, or pass
/// `vision_model: { endpoint, model }` to set/replace. If both are sent,
/// clear wins.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SettingsSaveDto {
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    /// When true, clear `default_provider` / `default_model`.
    #[serde(default)]
    pub clear_default_provider: bool,
    #[serde(default)]
    pub clear_default_model: bool,
    #[serde(default)]
    pub safety: Option<String>,
    #[serde(default)]
    pub vision_model: Option<VisionModelDto>,
    #[serde(default)]
    pub clear_vision_model: bool,
    /// Optional embedding model for memory recall.
    #[serde(default)]
    pub embedding_model: Option<EmbeddingModelDto>,
    #[serde(default)]
    pub clear_embedding_model: bool,
    /// Bundled in-process embedding model id (e.g. `"all-MiniLM-L6-v2"`).
    #[serde(default)]
    pub bundled_embedding_model: Option<String>,
    #[serde(default)]
    pub clear_bundled_embedding_model: bool,
    /// Laya classifier (opt-in): enable/disable it. Absent = keep the
    /// current value (see `GeneralSection::laya`).
    #[serde(default)]
    pub laya_enabled: Option<bool>,
    /// Laya `laya-serve` base URL, e.g. `"http://127.0.0.1:8000"`. Absent =
    /// keep; a blank string clears it (the classifier then reports
    /// unavailable when enabled).
    #[serde(default)]
    pub laya_endpoint: Option<String>,
    /// Laya provisioning mode: `external` (a user-run `laya-serve` at
    /// `laya_endpoint`) or `managed` (the app downloads + runs it — see
    /// Settings → Classifier). Absent = keep.
    #[serde(default)]
    pub laya_mode: Option<super::general::LayaMode>,
    /// Managed-mode checkpoint id (`"english"` | `"multilingual"`). Absent
    /// = keep; a blank string clears it to the `"english"` default.
    #[serde(default)]
    pub laya_checkpoint: Option<String>,
    /// Laya auto-typing of memory records (opt-in; enable only against a
    /// fine-tuned checkpoint — base checkpoints mis-classify). Absent =
    /// keep the current value.
    #[serde(default)]
    pub laya_auto_type_memories: Option<bool>,
    #[serde(default)]
    pub summarize_at_fill_rate: Option<f64>,
    /// Proxy cache ceiling in tokens (cliff guard). `Some(0)` clears it.
    #[serde(default)]
    pub proxy_cache_ceiling_tokens: Option<usize>,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub show_token_usage: Option<bool>,
    #[serde(default)]
    pub show_tool_images: Option<bool>,
    #[serde(default)]
    pub show_tool_activity: Option<bool>,
    #[serde(default)]
    pub show_knowledge_activity: Option<bool>,
    #[serde(default)]
    pub show_delegation_notes: Option<bool>,
    /// Per-kind steering-note display overrides (`[ui.steering_notes]`) — see
    /// [`SteeringNotesDto`].
    #[serde(default)]
    pub steering_notes: Option<SteeringNotesDto>,
    #[serde(default)]
    pub chat_thread_line: Option<bool>,
    #[serde(default)]
    pub chat_prose_cap: Option<bool>,
    #[serde(default)]
    pub chat_turn_tint: Option<bool>,
    #[serde(default)]
    pub chat_hover_timestamps: Option<bool>,
    #[serde(default)]
    pub sound_complete: Option<bool>,
    #[serde(default)]
    pub sound_input_needed: Option<bool>,
    #[serde(default)]
    pub sound_stopped_errors: Option<bool>,
    #[serde(default)]
    pub models: Option<ModelsConfigDto>,
    #[serde(default)]
    pub pricing: Option<Vec<PricingDto>>,
    #[serde(default)]
    pub skip_dirs: Option<Vec<String>>,
    #[serde(default)]
    pub core_operations: Option<Vec<String>>,
    #[serde(default)]
    pub enable_browser_inspection: Option<bool>,
    /// Patch `[general].auto_compact_on_plan_complete` — the Run-All
    /// between-items auto-compact (see `GeneralSection`'s docs).
    #[serde(default)]
    pub auto_compact_on_plan_complete: Option<bool>,
    #[serde(default)]
    pub trace_memory_budget_mb: Option<usize>,
    #[serde(default)]
    pub trace_request_body_cap_kb: Option<usize>,
    #[serde(default)]
    pub memory: Option<MemorySearchDto>,
}

// ── Validation + config assembly ──────────────────────────────────────────

/// Validate a settings patch against the current config and apply it,
/// returning the new [`Config`]. This is pure domain logic — no I/O, no Tauri
/// state. The caller persists + reloads + re-syncs the live provider.
///
/// Validation rejects out-of-range values with clear errors (rather than
/// silently clamping a typo), and cross-references vision/embedding/default
/// provider / `[models]` overrides against the current endpoints. On any
/// error the original config is unchanged (no partial application).
pub fn validate_and_apply_settings_patch(
    current: &Config,
    patch: &SettingsSaveDto,
) -> Result<Config, String> {
    // ── 1. Validate scalar fields ─────────────────────────────────────────
    if let Some(rate) = patch.summarize_at_fill_rate {
        if !(0.05..=0.95).contains(&rate) {
            return Err(format!(
                "summarize_at_fill_rate must be between 0.05 and 0.95 (got {rate})"
            ));
        }
    }
    if let Some(ceiling) = patch.proxy_cache_ceiling_tokens {
        if ceiling > 0 && ceiling < 65_536 {
            return Err(format!(
                "proxy_cache_ceiling_tokens must be 0 (disable) or at least 65_536 tokens (got {ceiling})"
            ));
        }
    }
    if let Some(mb) = patch.trace_memory_budget_mb {
        if !(1..=512).contains(&mb) {
            return Err(format!(
                "trace_memory_budget_mb must be between 1 and 512 (got {mb})"
            ));
        }
    }
    if let Some(kb) = patch.trace_request_body_cap_kb {
        if !(16..=8192).contains(&kb) {
            return Err(format!(
                "trace_request_body_cap_kb must be between 16 and 8192 (got {kb})"
            ));
        }
    }
    if let Some(memory) = &patch.memory {
        if let Some(d) = memory.decay_half_life_days {
            if !(d.is_finite() && (0.1..=365.0).contains(&d)) {
                return Err(format!(
                    "memory.decay_half_life_days must be between 0.1 and 365 (got {d})"
                ));
            }
        }
        if let Some(c) = memory.per_query_cap {
            if !(1..=500).contains(&c) {
                return Err(format!(
                    "memory.per_query_cap must be between 1 and 500 (got {c})"
                ));
            }
        }
        if let Some(c) = memory.derived_per_class_cap {
            if !(1..=100).contains(&c) {
                return Err(format!(
                    "memory.derived_per_class_cap must be between 1 and 100 (got {c})"
                ));
            }
        }
        for (name, budget) in [
            ("plan_budget", memory.plan_budget),
            ("bug_budget", memory.bug_budget),
            ("spec_budget", memory.spec_budget),
            ("decision_budget", memory.decision_budget),
            ("review_budget", memory.review_budget),
        ] {
            if let Some(b) = budget {
                if !(50..=5000).contains(&b) {
                    return Err(format!(
                        "memory.{name} must be between 50 and 5000 (got {b})"
                    ));
                }
            }
        }
    }
    if let Some(theme) = &patch.theme {
        let t = theme.to_ascii_lowercase();
        if t != "dark" && t != "light" && t != "system" {
            return Err(format!(
                "theme must be 'dark', 'light', or 'system' (got '{theme}')"
            ));
        }
    }
    let parsed_safety = match &patch.safety {
        Some(s) => Some(super::patch::parse_safety_mode(s)?),
        None => None,
    };
    if let Some(vm) = &patch.vision_model {
        if !patch.clear_vision_model {
            if vm.endpoint.trim().is_empty() {
                return Err("vision_model.endpoint must not be empty".into());
            }
            if vm.model.trim().is_empty() {
                return Err("vision_model.model must not be empty".into());
            }
        }
    }
    if let Some(pricing) = &patch.pricing {
        for p in pricing {
            if p.model.trim().is_empty() {
                return Err("pricing model name must not be empty".into());
            }
            if p.input_per_1m < 0.0 || p.output_per_1m < 0.0 || p.cached_per_1m < 0.0 {
                return Err(format!(
                    "pricing for '{}' must have non-negative rates",
                    p.model
                ));
            }
        }
    }

    // ── 2. Cross-reference validation against current endpoints ───────────
    let has_endpoints = !current.endpoints.is_empty();
    if let Some(dp) = &patch.default_provider {
        if !patch.clear_default_provider
            && has_endpoints
            && !current.endpoints.iter().any(|e| e.name == *dp)
        {
            return Err(format!(
                "default_provider '{dp}' does not match any configured endpoint"
            ));
        }
    }
    if let Some(vm) = &patch.vision_model {
        if !patch.clear_vision_model {
            let ep = vm.endpoint.trim();
            if has_endpoints && !current.endpoints.iter().any(|e| e.name == ep) {
                return Err(format!(
                    "vision_model.endpoint '{ep}' does not match any configured endpoint"
                ));
            }
        }
    }
    if let Some(em) = &patch.embedding_model {
        if !patch.clear_embedding_model {
            if em.endpoint.trim().is_empty() {
                return Err("embedding_model.endpoint must not be empty".into());
            }
            if em.model.trim().is_empty() {
                return Err("embedding_model.model must not be empty".into());
            }
            let ep = em.endpoint.trim();
            if has_endpoints && !current.endpoints.iter().any(|e| e.name == ep) {
                return Err(format!(
                    "embedding_model.endpoint '{ep}' does not match any configured endpoint"
                ));
            }
        }
    }
    if let Some(models) = &patch.models {
        let check = |m: &ModelRefDto| -> Result<(), String> {
            let ep = m.endpoint.trim();
            if ep.is_empty() {
                return Err("models override endpoint must not be empty".into());
            }
            if m.model.trim().is_empty() {
                return Err("models override model must not be empty".into());
            }
            if has_endpoints && !current.endpoints.iter().any(|e| e.name == ep) {
                return Err(format!(
                    "models override endpoint '{ep}' does not match any configured endpoint"
                ));
            }
            Ok(())
        };
        for field in [
            &models.planning,
            &models.executing,
            &models.bug_fixing,
            &models.reviewing,
            &models.complete,
            &models.subagent,
            &models.summarize,
        ] {
            if let Some(Some(m)) = field {
                check(m)?;
            }
        }
        if let Some(skill_map) = &models.skill {
            for m in skill_map.values() {
                check(m)?;
            }
        }
    }

    // ── 3. Apply patch into a new Config ──────────────────────────────────
    let mut general = current.general.clone();

    if patch.clear_default_provider {
        general.general.default_provider = None;
    } else if let Some(dp) = &patch.default_provider {
        general.general.default_provider = Some(dp.clone());
    }
    if patch.clear_default_model {
        general.general.default_model = None;
    } else if let Some(dm) = &patch.default_model {
        general.general.default_model = Some(dm.clone());
    }
    if let Some(mode) = parsed_safety {
        general.general.safety = mode;
    }
    if patch.clear_vision_model {
        general.general.vision_model = None;
    } else if let Some(vm) = &patch.vision_model {
        general.general.vision_model = Some(super::general::VisionModel {
            endpoint: vm.endpoint.trim().to_string(),
            model: vm.model.trim().to_string(),
        });
    }
    if patch.clear_embedding_model {
        general.general.embedding_model = None;
    } else if let Some(em) = &patch.embedding_model {
        general.general.embedding_model = Some(super::general::EmbeddingModel {
            endpoint: em.endpoint.trim().to_string(),
            model: em.model.trim().to_string(),
        });
    }
    if patch.clear_bundled_embedding_model {
        general.general.bundled_embedding_model =
            Some(crate::config::EMBEDDING_MODEL_SENTINEL_HASH.to_string());
    } else if let Some(bm) = &patch.bundled_embedding_model {
        let trimmed = bm.trim();
        if !trimmed.is_empty() {
            general.general.bundled_embedding_model = Some(trimmed.to_string());
        }
    }
    if let Some(enabled) = patch.laya_enabled {
        general.general.laya.enabled = enabled;
    }
    if let Some(endpoint) = &patch.laya_endpoint {
        let trimmed = endpoint.trim();
        general.general.laya.endpoint = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Some(mode) = patch.laya_mode {
        general.general.laya.mode = mode;
    }
    if let Some(checkpoint) = &patch.laya_checkpoint {
        let trimmed = checkpoint.trim();
        general.general.laya.checkpoint = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Some(auto_type) = patch.laya_auto_type_memories {
        general.general.laya.auto_type_memories = auto_type;
    }
    if let Some(rate) = patch.summarize_at_fill_rate {
        general.context.summarize_at_fill_rate = rate;
    }
    if let Some(ceiling) = patch.proxy_cache_ceiling_tokens {
        general.context.proxy_cache_ceiling_tokens = if ceiling == 0 { None } else { Some(ceiling) };
    }
    if let Some(mb) = patch.trace_memory_budget_mb {
        general.trace.memory_budget_mb = super::TraceConfig::clamp_budget(mb);
    }
    if let Some(kb) = patch.trace_request_body_cap_kb {
        general.trace.request_body_cap_kb = super::TraceConfig::clamp_request_cap(kb);
    }
    if let Some(memory) = &patch.memory {
        let cur = &mut general.memory;
        if let Some(v) = memory.decay_half_life_days {
            cur.decay_half_life_days = v;
        }
        if let Some(v) = memory.per_query_cap {
            cur.per_query_cap = v;
        }
        if let Some(v) = memory.derived_per_class_cap {
            cur.derived_per_class_cap = v;
        }
        if let Some(v) = memory.plan_budget {
            cur.plan_budget = v;
        }
        if let Some(v) = memory.bug_budget {
            cur.bug_budget = v;
        }
        if let Some(v) = memory.spec_budget {
            cur.spec_budget = v;
        }
        if let Some(v) = memory.decision_budget {
            cur.decision_budget = v;
        }
        if let Some(v) = memory.review_budget {
            cur.review_budget = v;
        }
    }
    if let Some(theme) = &patch.theme {
        general.ui.theme = theme.to_ascii_lowercase();
    }
    if let Some(show) = patch.show_token_usage {
        general.ui.show_token_usage = show;
    }
    if let Some(show) = patch.show_tool_images {
        general.ui.show_tool_images = show;
    }
    if let Some(show) = patch.show_tool_activity {
        general.ui.show_tool_activity = show;
    }
    if let Some(show) = patch.show_knowledge_activity {
        general.ui.show_knowledge_activity = show;
    }
    if let Some(show) = patch.show_delegation_notes {
        general.ui.show_delegation_notes = show;
    }
    if let Some(notes) = &patch.steering_notes {
        let cur = &mut general.ui.steering_notes;
        if let Some(v) = notes.auto_delegated {
            cur.auto_delegated = Some(v);
        }
        if let Some(v) = notes.search_nudge {
            cur.search_nudge = Some(v);
        }
        if let Some(v) = notes.shell_tip {
            cur.shell_tip = Some(v);
        }
        if let Some(v) = notes.graph_miss {
            cur.graph_miss = Some(v);
        }
        if let Some(v) = notes.recall_rider {
            cur.recall_rider = Some(v);
        }
        if let Some(v) = notes.read_nudge {
            cur.read_nudge = Some(v);
        }
        if let Some(v) = notes.literal_tip {
            cur.literal_tip = Some(v);
        }
        if let Some(v) = notes.known_memory_hit {
            cur.known_memory_hit = Some(v);
        }
        if let Some(v) = notes.consolidation_due {
            cur.consolidation_due = Some(v);
        }
        if let Some(v) = notes.shell_redirect {
            cur.shell_redirect = Some(v);
        }
        if let Some(v) = notes.edit_stale_read {
            cur.edit_stale_read = Some(v);
        }
    }
    if let Some(on) = patch.chat_thread_line {
        general.ui.chat_thread_line = on;
    }
    if let Some(on) = patch.chat_prose_cap {
        general.ui.chat_prose_cap = on;
    }
    if let Some(on) = patch.chat_turn_tint {
        general.ui.chat_turn_tint = on;
    }
    if let Some(on) = patch.chat_hover_timestamps {
        general.ui.chat_hover_timestamps = on;
    }
    if let Some(on) = patch.sound_complete {
        general.ui.sound_complete = on;
    }
    if let Some(on) = patch.sound_input_needed {
        general.ui.sound_input_needed = on;
    }
    if let Some(on) = patch.sound_stopped_errors {
        general.ui.sound_stopped_errors = on;
    }
    if let Some(enabled) = patch.enable_browser_inspection {
        general.general.enable_browser_inspection = enabled;
    }
    if let Some(on) = patch.auto_compact_on_plan_complete {
        general.general.auto_compact_on_plan_complete = on;
    }
    if let Some(models) = &patch.models {
        let to_ref = |m: &ModelRefDto| ModelRef {
            endpoint: m.endpoint.trim().to_string(),
            model: m.model.trim().to_string(),
            reasoning_effort: m
                .reasoning_effort
                .as_deref()
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_string),
        };
        if let Some(p) = &models.planning {
            general.models.planning = p.as_ref().map(|m| to_ref(m));
        }
        if let Some(e) = &models.executing {
            general.models.executing = e.as_ref().map(|m| to_ref(m));
        }
        if let Some(b) = &models.bug_fixing {
            general.models.bug_fixing = b.as_ref().map(|m| to_ref(m));
        }
        if let Some(r) = &models.reviewing {
            general.models.reviewing = r.as_ref().map(|m| to_ref(m));
        }
        if let Some(c) = &models.complete {
            general.models.complete = c.as_ref().map(|m| to_ref(m));
        }
        if let Some(s) = &models.subagent {
            general.models.subagent = s.as_ref().map(|m| to_ref(m));
        }
        if let Some(s) = &models.summarize {
            general.models.summarize = s.as_ref().map(|m| to_ref(m));
        }
        if let Some(skill_map) = &models.skill {
            general.models.skill = skill_map
                .iter()
                .map(|(k, v)| (k.clone(), to_ref(v)))
                .collect();
        }
    }

    let pricing = if let Some(rows) = &patch.pricing {
        rows.iter()
            .map(|p| super::PricingEntry {
                model: p.model.trim().to_string(),
                input_per_1m: p.input_per_1m,
                output_per_1m: p.output_per_1m,
                cached_per_1m: p.cached_per_1m,
            })
            .collect()
    } else {
        current.pricing.clone()
    };

    if let Some(sd) = &patch.skip_dirs {
        general.markdown.skip_dirs = sd
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    if let Some(ops) = &patch.core_operations {
        general.git.core_operations = ops
            .iter()
            .map(|s| s.trim().to_lowercase().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    Ok(Config {
        general,
        endpoints: current.endpoints.clone(),
        pricing,
        keys: current.keys.clone(),
        mcp: current.mcp.clone(),
        projects: current.projects.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_readability_flags_persist_through_the_save_patch() {
        // Regression (review H1, plan afa81f0a): the four chat-readability
        // toggles must ride the save path — a patch carrying them flips the
        // UiConfig values. Before the fix, serde silently dropped the
        // unknown keys and every toggle reverted on restart.
        let current = Config::default();
        let patch = SettingsSaveDto {
            chat_thread_line: Some(false),
            chat_prose_cap: Some(false),
            chat_turn_tint: Some(false),
            chat_hover_timestamps: Some(false),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert!(!next.general.ui.chat_thread_line);
        assert!(!next.general.ui.chat_prose_cap);
        assert!(!next.general.ui.chat_turn_tint);
        assert!(!next.general.ui.chat_hover_timestamps);
        // Untouched fields keep their defaults.
        assert!(next.general.ui.sound_complete);
    }

    #[test]
    fn steering_notes_patch_applies_per_kind() {
        // The per-kind steering-note overrides ride the save path: Some flips
        // just that kind's override, None (absent) leaves the other kinds —
        // and the legacy show_delegation_notes — untouched.
        let current = Config::default();
        let patch = SettingsSaveDto {
            show_delegation_notes: Some(true),
            steering_notes: Some(SteeringNotesDto {
                auto_delegated: Some(false),
                literal_tip: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert_eq!(next.general.ui.steering_notes.auto_delegated, Some(false));
        assert_eq!(next.general.ui.steering_notes.literal_tip, Some(false));
        // Only the patched kinds gained an override.
        assert_eq!(next.general.ui.steering_notes.search_nudge, None);
        // Resolution honors both: the explicit per-kind value beats the legacy
        // toggle, untouched kinds keep their defaults.
        assert!(!next.general.ui.effective_steering_note_visible("auto_delegated"));
        assert!(!next.general.ui.effective_steering_note_visible("literal_tip"));
        assert!(next.general.ui.effective_steering_note_visible("search_nudge"));
        // An absent steering_notes patch changes nothing.
        let patch = SettingsSaveDto::default();
        let after = validate_and_apply_settings_patch(&next, &patch).unwrap();
        assert_eq!(after.general.ui.steering_notes, next.general.ui.steering_notes);
    }

    #[test]
    fn auto_compact_on_plan_complete_patch_applies() {
        // The Run-All auto-compact flag rides the save patch: Some flips the
        // value in both directions; None (absent) keeps the current value.
        let current = Config::default();
        let patch = SettingsSaveDto {
            auto_compact_on_plan_complete: Some(true),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert!(next.general.general.auto_compact_on_plan_complete);
        // Flip back off.
        let patch = SettingsSaveDto {
            auto_compact_on_plan_complete: Some(false),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&next, &patch).unwrap();
        assert!(!next.general.general.auto_compact_on_plan_complete);
        // Absent leaves the current value (here: false).
        let patch = SettingsSaveDto::default();
        let next = validate_and_apply_settings_patch(&next, &patch).unwrap();
        assert!(!next.general.general.auto_compact_on_plan_complete);
    }

    #[test]
    fn laya_patch_applies_enabled_and_endpoint() {
        // The opt-in Laya classifier rides the save path: Some flips the
        // enable flag, Some(endpoint) sets the base URL (trimmed), a blank
        // endpoint clears it, and absent fields keep the current values.
        let current = Config::default();
        let patch = SettingsSaveDto {
            laya_enabled: Some(true),
            laya_endpoint: Some("  http://127.0.0.1:8000  ".into()),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert!(next.general.general.laya.enabled);
        assert_eq!(
            next.general.general.laya.endpoint.as_deref(),
            Some("http://127.0.0.1:8000")
        );
        // A blank endpoint clears it; the enable flag is untouched.
        let patch = SettingsSaveDto {
            laya_endpoint: Some("   ".into()),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&next, &patch).unwrap();
        assert!(next.general.general.laya.enabled);
        assert_eq!(next.general.general.laya.endpoint, None);
        // Defaults: nothing set, nothing enabled.
        let empty = Config::default();
        assert!(!empty.general.general.laya.enabled);
        assert!(empty.general.general.laya.endpoint.is_none());
    }

    #[test]
    fn laya_patch_applies_mode_and_checkpoint() {
        // Managed-mode fields ride the same save path: Some(mode) flips the
        // provisioning mode, Some(checkpoint) sets it (trimmed), a blank
        // checkpoint clears it to the english default, absent keeps current.
        let current = Config::default();
        assert_eq!(
            current.general.general.laya.mode,
            crate::config::LayaMode::External
        );
        let patch = SettingsSaveDto {
            laya_mode: Some(crate::config::LayaMode::Managed),
            laya_checkpoint: Some("  multilingual  ".into()),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert_eq!(next.general.general.laya.mode, crate::config::LayaMode::Managed);
        assert_eq!(
            next.general.general.laya.checkpoint.as_deref(),
            Some("multilingual")
        );
        // A blank checkpoint clears it; the mode is untouched.
        let patch = SettingsSaveDto {
            laya_checkpoint: Some("   ".into()),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&next, &patch).unwrap();
        assert_eq!(next.general.general.laya.mode, crate::config::LayaMode::Managed);
        assert_eq!(next.general.general.laya.checkpoint, None);
    }

    #[test]
    fn laya_patch_applies_auto_typing() {
        // The auto-typing opt-in rides the same save path: Some flips it,
        // absent keeps the current value.
        let current = Config::default();
        assert!(!current.general.general.laya.auto_type_memories);
        let patch = SettingsSaveDto {
            laya_auto_type_memories: Some(true),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert!(next.general.general.laya.auto_type_memories);
        // Absent keeps it on.
        let patch = SettingsSaveDto::default();
        let next = validate_and_apply_settings_patch(&next, &patch).unwrap();
        assert!(next.general.general.laya.auto_type_memories);
    }

    #[test]
    fn proxy_cache_ceiling_validation_rejects_tiny_and_accepts_disable_or_sane() {
        // Ported from the removed patch.rs twin (quality review HIGH 3): a
        // tiny ceiling would summarize constantly — meaningless as a cliff
        // guard, so it is rejected. 0 (disable) and sane values pass, and 0
        // clears the stored guard to None.
        let current = Config::default();
        let patch = SettingsSaveDto {
            proxy_cache_ceiling_tokens: Some(1024),
            ..Default::default()
        };
        let r = validate_and_apply_settings_patch(&current, &patch).unwrap_err();
        assert!(r.contains("proxy_cache_ceiling_tokens"));
        let patch = SettingsSaveDto {
            proxy_cache_ceiling_tokens: Some(340_000),
            ..Default::default()
        };
        assert!(validate_and_apply_settings_patch(&current, &patch).is_ok());
        let patch = SettingsSaveDto {
            proxy_cache_ceiling_tokens: Some(0),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert_eq!(next.general.context.proxy_cache_ceiling_tokens, None);
    }

    #[test]
    fn models_patch_double_option_semantics() {
        // Ported from the removed patch.rs twin (quality review HIGH 3): the
        // [models] overrides are double-Option — outer None keeps the
        // existing value, Some(None) clears it, Some(Some(m)) sets it.
        let mut current = Config::default();
        current.general.models.planning = Some(ModelRef {
            endpoint: "ep".into(),
            model: "old".into(),
            reasoning_effort: None,
        });
        // Set: Some(Some(m)) replaces.
        let patch = SettingsSaveDto {
            models: Some(ModelsConfigDto {
                planning: Some(Some(ModelRefDto {
                    endpoint: "ep2".into(),
                    model: "m2".into(),
                    reasoning_effort: None,
                })),
                ..Default::default()
            }),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert_eq!(
            next.general.models.planning,
            Some(ModelRef {
                endpoint: "ep2".into(),
                model: "m2".into(),
                reasoning_effort: None,
            })
        );
        // Clear: Some(None) → None.
        let patch = SettingsSaveDto {
            models: Some(ModelsConfigDto {
                planning: Some(None),
                ..Default::default()
            }),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert_eq!(next.general.models.planning, None);
        // Keep: outer None → unchanged.
        let patch = SettingsSaveDto {
            models: Some(ModelsConfigDto::default()),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert_eq!(
            next.general.models.planning,
            Some(ModelRef {
                endpoint: "ep".into(),
                model: "old".into(),
                reasoning_effort: None,
            })
        );
    }

    #[test]
    fn bug_fixing_patch_set_clear_keep() {
        // 2027-01-07: the bug-fixing slot rides the [models] patch like the
        // other slots — Some(Some(m)) replaces, Some(None) clears, outer None
        // keeps — and validates against the configured endpoints.
        let mut current = Config::default();
        current.general.models.bug_fixing = Some(ModelRef {
            endpoint: "ep".into(),
            model: "old".into(),
            reasoning_effort: None,
        });
        // Set: Some(Some(m)) replaces (effort included).
        let patch = SettingsSaveDto {
            models: Some(ModelsConfigDto {
                bug_fixing: Some(Some(ModelRefDto {
                    endpoint: "ep2".into(),
                    model: "m2".into(),
                    reasoning_effort: Some("max".into()),
                })),
                ..Default::default()
            }),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert_eq!(
            next.general.models.bug_fixing,
            Some(ModelRef {
                endpoint: "ep2".into(),
                model: "m2".into(),
                reasoning_effort: Some("max".into()),
            })
        );
        // Clear: Some(None) → None.
        let patch = SettingsSaveDto {
            models: Some(ModelsConfigDto {
                bug_fixing: Some(None),
                ..Default::default()
            }),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert_eq!(next.general.models.bug_fixing, None);
        // Keep: outer None → unchanged.
        let patch = SettingsSaveDto {
            models: Some(ModelsConfigDto::default()),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        assert_eq!(
            next.general.models.bug_fixing,
            Some(ModelRef {
                endpoint: "ep".into(),
                model: "old".into(),
                reasoning_effort: None,
            })
        );
    }

    #[test]
    fn models_patch_carries_reasoning_effort() {
        // The per-context effort rides the [models] patch end-to-end; absent
        // or whitespace-only effort normalizes to None (model default).
        let current = Config::default();
        let patch = SettingsSaveDto {
            models: Some(ModelsConfigDto {
                planning: Some(Some(ModelRefDto {
                    endpoint: "ep".into(),
                    model: "m".into(),
                    reasoning_effort: Some("low".into()),
                })),
                ..Default::default()
            }),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        let p = next.general.models.planning.expect("planning set");
        assert_eq!(p.reasoning_effort.as_deref(), Some("low"));

        let patch = SettingsSaveDto {
            models: Some(ModelsConfigDto {
                executing: Some(Some(ModelRefDto {
                    endpoint: "ep".into(),
                    model: "m".into(),
                    reasoning_effort: Some("   ".into()),
                })),
                ..Default::default()
            }),
            ..Default::default()
        };
        let next = validate_and_apply_settings_patch(&current, &patch).unwrap();
        let e = next.general.models.executing.expect("executing set");
        assert_eq!(e.reasoning_effort, None);
    }
}
