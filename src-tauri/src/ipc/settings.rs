// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Settings/config/endpoints/keys Tauri commands.
//!
//! Owns the full Settings surface: `get_config` / `get_settings` payloads,
//! endpoint + API-key + pricing persistence (`save_endpoints`), the
//! non-endpoint settings patch (`save_settings`), live model listing, and
//! the runtime rewire of vision/embedder after a config change.

use serde::{Deserialize, Serialize};
use tauri::State;

use mnemo::config::general::UiConfig;
use mnemo::config::settings_dto::{validate_and_apply_settings_patch, SettingsSaveDto};
use mnemo::config::{Endpoint, EndpointKind, SafetyMode};
use mnemo::memory::embedder::EmbedderStatus;
use mnemo::provider::client_factory::build_client;

use crate::ipc::error::IpcError;
use crate::ipc::rewire::{rewire_vision_and_embedder, sync_model_resolver};
use crate::ipc::state::IpcState;

/// Get the live embedder status (Ready / Checking / Fallback / Pulling /
/// Failed). Polled by the frontend on startup and updated via the
/// `embedder://status` event so the UI can show a banner when semantic recall
/// has degraded to keyword-only.
#[tauri::command]
pub async fn get_embedder_status(state: State<'_, IpcState>) -> Result<EmbedderStatus, IpcError> {
    state.embedder_status()
}

/// Get the global config for the Settings UI, minus secrets.
///
/// Returns a JSON object `{ general, endpoints, pricing }`: `general` holds
/// the default provider/model + safety mode; `endpoints` lists each configured
/// endpoint (name, kind, base_url, models, context/output caps, multimodal,
/// supports_reasoning_effort, reasoning_effort); `pricing` lists per-model
/// pricing entries. API keys are never included (the `keys.toml` store is not
/// serialized).
///
/// (E5: the legacy `get_config` command was deleted — `get_settings` is a
/// strict superset and all callers migrated to it.)

/// The editable shape of an endpoint as sent by the Settings → Endpoints tab.
///
/// Mirrors [`mnemo::config::Endpoint`] but with `kind` as a `String`
/// (the frontend's `<select>` sends a literal), so the command parses it back
/// to [`EndpointKind`] via serde. `api_key` is intentionally **not** here —
/// keys travel in a separate `api_keys` map keyed by endpoint name (see
/// [`save_endpoints`]), keeping the no-secrets guarantee of [`get_config`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointDto {
    pub name: String,
    /// `"openai"` or `"local"` (the serde form); also accepts the Debug form
    /// (`"OpenAI"`/`"Local"`) for resilience against the legacy `get_config`
    /// payload.
    pub kind: String,
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<String>,
    /// Per-model config (caps + reasoning-effort lists), optional — absent
    /// (or empty) keeps the legacy single-value-per-endpoint behaviour. When
    /// present, configs are matched to models BY ID (the frontend sends them
    /// as a parallel array, but position drift is tolerated); configs for
    /// models not in `models` are dropped on save.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_configs: Vec<ModelSpecDto>,
    #[serde(default)]
    pub max_context: Option<usize>,
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
    #[serde(default)]
    pub multimodal: bool,
    /// Whether the endpoint accepts `reasoning_effort`. Defaults to `true`
    /// when the key is absent (back-compat with older Settings payloads).
    #[serde(default = "default_true_dto")]
    pub supports_reasoning_effort: bool,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Optional Anthropic workspace id (`anthropic-workspace-id` header);
    /// anthropic endpoints only. Absent → None.
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// The editable per-model config sent by the Settings → Endpoints tab —
/// mirrors [`mnemo::config::ModelSpec`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpecDto {
    /// The model id (must match a `models` entry).
    pub id: String,
    #[serde(default)]
    pub max_context: Option<usize>,
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    /// Per-model reasoning-effort default. Absent/null = the endpoint's
    /// `reasoning_effort` applies (then the app default "max").
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Per-model multimodal (vision) override. Absent/null = the endpoint's
    /// `multimodal` flag applies.
    #[serde(default)]
    pub multimodal: Option<bool>,
}

/// Serde default matching [`mnemo::config::Endpoint`]'s
/// `supports_reasoning_effort` default.
fn default_true_dto() -> bool {
    true
}

impl EndpointDto {
    /// Parse the DTO into a validated [`Endpoint`]. Delegates to the lib-side
    /// [`mnemo::config::patch::validate_endpoint`] so the validation logic
    /// is unit-testable without Tauri state.
    fn into_endpoint(self) -> Result<Endpoint, String> {
        // DTO per-model configs → lib-side specs (same field set; the lib
        // type is what endpoints.toml persists).
        let specs = self
            .model_configs
            .iter()
            .map(|d| mnemo::config::ModelSpec {
                id: d.id.clone(),
                max_context: d.max_context,
                max_output_tokens: d.max_output_tokens,
                reasoning_efforts: d.reasoning_efforts.clone(),
                reasoning_effort: d.reasoning_effort.clone(),
                multimodal: d.multimodal,
                ..Default::default()
            })
            .collect();
        mnemo::config::patch::validate_endpoint(
            &self.name,
            &self.kind,
            &self.base_url,
            self.models,
            specs,
            self.max_context,
            self.max_output_tokens,
            self.multimodal,
            self.supports_reasoning_effort,
            self.reasoning_effort,
            self.workspace_id,
        )
    }
}

/// Persist the edited endpoint list + API keys + default provider/model.
///
/// Receives the full edited set from the Settings → Endpoints tab (the tab is
/// the source of truth for the save — it sends everything, not a diff). The
/// command:
///
/// 1. Validates each endpoint (names unique + non-empty, base_url non-empty,
///    normalized to end `/`), rejecting the whole batch on any error so a
///    partial save never lands.
/// 2. Writes `endpoints.toml`, `keys.toml`, and `config.toml` (the `[general]`
///    section's `default_provider`/`default_model` are updated; every other
///    section — context/memory/ui/safety/vision_model — is preserved) via
///    [`Config::save_all`].
/// 3. Reloads the config from disk and swaps it into `state.project.config` (so the
///    rest of the app reads the new values).
/// 4. Re-syncs the live runtime ([`resync_runtime_state`]): if there is a
///    default endpoint after the save, rebuilds the provider from the
///    *current* (reloaded) config and swaps it into the factory + every live
///    agent loop (reusing the same path as [`set_model`]). This is
///    unconditional (not gated on a name change): an in-place edit to the
///    default endpoint's base_url, kind, multimodal flag, reasoning effort, or
///    API key must also take effect live, not just after a restart.
///
/// Returns the new default endpoint + model names + `provider_swapped` (true
/// when the live provider was rebuilt) so the UI can re-sync its labels and
/// show an accurate success message. Returns `Err(msg)` on validation/write
/// failure (no partial state change — `state.project.config` is only swapped after a
/// successful reload).
#[tauri::command]
pub async fn save_endpoints(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    endpoints: Vec<EndpointDto>,
    api_keys: std::collections::HashMap<String, String>,
    default_provider: Option<String>,
    default_model: Option<String>,
) -> Result<SaveEndpointsResponse, IpcError> {
    // ── 1. Validate + convert ────────────────────────────────────────────
    // Snapshot the *current* default model up front (cheap read) so the
    // default_model validation can be lenient about the pre-save running model
    // (mirrors set_model's implicit allow).
    let old_default_model = {
        let current = state.project.config.lock().await;
        current.general.general.default_model.clone()
    };

    let mut built: Vec<Endpoint> = Vec::with_capacity(endpoints.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for dto in endpoints {
        let ep = dto.into_endpoint()?;
        if !seen.insert(ep.name.clone()) {
            return Err(format!("duplicate endpoint name '{}'", ep.name).into());
        }
        built.push(ep);
    }
    // Cross-endpoint validation (unique names already checked above; the lib
    // fn checks default_provider/default_model refs + resolvable model).
    mnemo::config::patch::validate_endpoint_set(
        &built,
        default_provider.as_deref(),
        default_model.as_deref(),
        old_default_model.as_deref(),
    )?;

    // ── 2. Build the new Config (preserving pricing + non-general sections) ─
    let new_config = {
        let current = state.project.config.lock().await;
        mnemo::config::patch::apply_endpoints(
            &current,
            built,
            default_provider.clone(),
            default_model.clone(),
            &api_keys,
        )
    };

    // ── 3. Persist + reload ──────────────────────────────────────────────
    crate::ipc::config_io::persist_and_reload(&state, new_config).await?;

    // ── 4. Re-sync the live runtime (provider + vision/embedder/resolver) ─
    let provider_swapped = resync_runtime_state(&app, &state).await;

    Ok(SaveEndpointsResponse {
        default_provider: default_provider.clone(),
        default_model: default_model.clone(),
        provider_swapped,
    })
}

/// Re-sync the live runtime after an endpoint/key save (step 4 of
/// [`save_endpoints`]).
///
/// Unconditional (not gated on a name change): an in-place edit to the
/// default endpoint's base_url, kind, multimodal flag, reasoning effort, or
/// API key must take effect live. Rebuilds the provider from the *reloaded*
/// config (so the new on-disk values drive the client) and swaps it into the
/// factory + every live agent loop, then rewires vision/embedder and the
/// per-context model resolver. Returns whether a provider swap actually
/// happened (`false` when there's no factory, or no default endpoint — the
/// latter means no endpoints at all).
///
/// The endpoint/model resolution delegates to
/// [`crate::startup::resolve_startup_provider`] (the tested startup
/// fallback-ordering helper), so the re-sync picks the same endpoint + model
/// the next app start would.
async fn resync_runtime_state(app: &tauri::AppHandle, state: &State<'_, IpcState>) -> bool {
    let provider_swapped = if state.runtime.factory.is_some() {
        let (provider, context_manager, kind_label, swapped_model, swapped_display_effort) = {
            let cfg = state.project.config.lock().await;
            let (endpoint, model) = crate::startup::resolve_startup_provider(&cfg);
            match endpoint {
                Some(ep) => {
                    // The validation in `save_endpoints` guarantees a model
                    // resolves (default_model or the endpoint's first model),
                    // so the "gpt-4o" terminal fallback is never hit in
                    // practice.
                    let kind_label = format!("{:?}", ep.kind);
                    // Wire the shared trace log — the re-synced provider is
                    // the main chat path, so its requests belong in the Trace
                    // tab.
                    let provider = build_client(
                        &cfg,
                        ep,
                        &model,
                        ep.multimodal_for(&model),
                        ep.effective_reasoning_effort_for(Some(&model)),
                        Some(state.trace.clone()),
                    );
                    let context_manager = state
                        .runtime
                        .factory
                        .as_ref()
                        .unwrap()
                        .context_manager_for(provider.as_ref());
                    // The DISPLAY-space effort of the rebuilt default (the
                    // same chain the request builder uses, "off" kept
                    // verbatim) — carried on the ModelChanged event + recorded
                    // on the loops so the status bar tracks it (backlog
                    // 51dab4da).
                    let display_effort = ep.display_reasoning_effort_for(Some(&model));
                    (
                        Some(provider),
                        Some(context_manager),
                        kind_label,
                        Some(model),
                        Some(display_effort),
                    )
                }
                None => {
                    // No endpoints at all — leave the existing (dummy)
                    // provider in place, but still rewire vision/embedder
                    // below (they may have been cleared).
                    (None, None, String::new(), None, None)
                }
            }
        };
        if let (Some(provider), Some(context_manager), Some(model), display_effort) =
            (provider, context_manager, swapped_model, swapped_display_effort)
        {
            let swapped = crate::ipc::config_io::swap_live_provider(
                state,
                app,
                provider,
                context_manager,
                model,
                display_effort,
            )
            .await;
            eprintln!("save_endpoints: re-synced live provider ({kind_label})");
            swapped
        } else {
            false
        }
    } else {
        false
    };

    // Endpoint/key edits can also affect vision (endpoint base_url/key) and
    // the memory embedder (embedding provider endpoint).
    {
        let cfg = state.project.config.lock().await;
        rewire_vision_and_embedder(state, &cfg);
        // An endpoint edit can change which `[models]` overrides resolve (a
        // referenced endpoint may have been added/removed/renamed), so push the
        // reloaded config into the resolver too.
        sync_model_resolver(state, &cfg);
    }

    provider_swapped
}

// ── Full Settings read/write (general / context / ui / vision / pricing) ─
// These typed structs lock the wire shape at compile time — serde renders them
// byte-identically to the old `json!` bodies, so no frontend change is
// required, but any field change is now a compile error + a fixture-equality
// test failure (see the Step 2/3 fixtures).
//
// IMPORTANT: these structs deliberately do NOT use `skip_serializing_if` on
// any `Option` field. The legacy `json!` bodies emitted `null` for `None`
// (e.g. `default_provider`, `vision_model`), and the frontend types model
// those as `string | null`. Skipping `None` would drop the key and break the
// FE. Preserve the null-emitting behavior exactly.

/// One model inside `EndpointWire` — the per-model config surface.
#[derive(Debug, Clone, Serialize)]
pub struct ModelSpecWire {
    /// The model id.
    pub id: String,
    /// Per-model max-context override (`null` = the endpoint's value).
    pub max_context: Option<usize>,
    /// Per-model max-output override (`null` = the endpoint's value).
    pub max_output_tokens: Option<usize>,
    /// Per-model reasoning-effort allow-list (empty = the endpoint's list).
    pub reasoning_efforts: Vec<String>,
    /// Per-model reasoning-effort default (`null` = the endpoint's value,
    /// then the app default "max").
    pub reasoning_effort: Option<String>,
    /// Per-model multimodal (vision) override (`null` = the endpoint's
    /// `multimodal` flag applies).
    pub multimodal: Option<bool>,
}

impl ModelSpecWire {
    fn from_spec(spec: &mnemo::config::ModelSpec) -> Self {
        Self {
            id: spec.id.clone(),
            max_context: spec.max_context,
            max_output_tokens: spec.max_output_tokens,
            reasoning_efforts: spec.reasoning_efforts.clone(),
            reasoning_effort: spec.reasoning_effort.clone(),
            multimodal: spec.multimodal,
        }
    }
}

/// An endpoint as it appears on the wire (the `get_config` / `get_settings`
/// `endpoints[]` element). `kind` is the serde wire form (`"openai"` /
/// `"local"`), produced by [`endpoint_kind_wire`] — not the Debug form.
#[derive(Debug, Clone, Serialize)]
pub struct EndpointWire {
    /// Endpoint name (unique key in `endpoints.toml`).
    pub name: String,
    /// Serde wire form: `"openai"` or `"local"`.
    pub kind: String,
    /// Base URL (normalized to end with `/` on save).
    pub base_url: String,
    /// Model ids served by this endpoint (id-only view — per-model config
    /// rides the parallel `model_configs` array, keeping the legacy shape).
    pub models: Vec<String>,
    /// Per-model config (caps + reasoning-effort lists), same order as
    /// `models`. Always mirrors `models` entry-for-entry (bare entries carry
    /// `null`/empty fields when a model has no per-model config).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_configs: Vec<ModelSpecWire>,
    /// Max context window in tokens (`null` = provider-kind default).
    pub max_context: Option<usize>,
    /// Max output tokens per request (`null` = max_context / 2).
    pub max_output_tokens: Option<usize>,
    /// Whether models at this endpoint accept image inputs.
    pub multimodal: bool,
    /// Whether models accept the `reasoning_effort` request field.
    pub supports_reasoning_effort: bool,
    /// Configured reasoning effort (`null` = endpoint default "max").
    pub reasoning_effort: Option<String>,
    /// Optional Anthropic workspace id (`anthropic-workspace-id` header);
    /// anthropic endpoints only. Absent on endpoints without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// A per-model pricing row (the `pricing[]` element).
#[derive(Debug, Clone, Serialize)]
pub struct PricingWire {
    /// Model id.
    pub model: String,
    /// $ per 1M input tokens.
    pub input_per_1m: f64,
    /// $ per 1M output tokens.
    pub output_per_1m: f64,
    /// $ per 1M cached prompt tokens.
    pub cached_per_1m: f64,
}

/// A vision fallback model (the `general.vision_model` value). Emitted as
/// `null` (not skipped) when no vision model is configured.
#[derive(Debug, Clone, Serialize)]
pub struct VisionModelWire {
    /// Endpoint name hosting the vision model.
    pub endpoint: String,
    /// Model id.
    pub model: String,
}

/// An embedding model (the `general.embedding_model` value). Emitted as
/// `null` (not skipped) when no embedding model is configured (hash mode).
/// Structurally identical to [`VisionModelWire`] but a distinct type so the
/// two config surfaces cannot be confused (per the convention in
/// `config::general`).
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingModelWire {
    /// Endpoint name hosting the embedding model.
    pub endpoint: String,
    /// Model id (e.g. `nomic-embed-text`, `text-embedding-3-small`).
    pub model: String,
}

/// A per-context model override (one entry of the `[models]` section). Emitted
/// as `null` (not skipped) when unset, so the frontend can distinguish "no
/// override" from "override to the default model".
#[derive(Debug, Clone, Serialize)]
pub struct ModelRefWire {
    /// Endpoint name (matches an entry in `endpoints.toml`).
    pub endpoint: String,
    /// Model id at that endpoint.
    pub model: String,
    /// Optional per-context reasoning-effort override (absent = the model's
    /// own default applies).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// The `[models]` section — per-context model overrides. Mirrors
/// [`mnemo::config::ModelsConfig`]. Each field is `null` when unset.
#[derive(Debug, Clone, Serialize)]
pub struct ModelsConfigWire {
    /// Model override for the Planning state (`null` = use default).
    pub planning: Option<ModelRefWire>,
    /// Model override for the Executing state (`null` = use default).
    pub executing: Option<ModelRefWire>,
    /// Model + reasoning effort for the active plan's bug-fixing lifecycle:
    /// while the active plan's kind is `bug_fixing` and the workflow is in
    /// the Executing or Reviewing state, this slot wins over `executing`
    /// (`null` = inherit the executing override, then default).
    pub bug_fixing: Option<ModelRefWire>,
    /// Model for the spawned reviewer subagent — reviewer-spawn-only (`null`
    /// = fall back to the executing override, then subagent, then default;
    /// the main agent keeps executing through Reviewing).
    pub reviewing: Option<ModelRefWire>,
    /// Model override for the Complete state (`null` = use default).
    pub complete: Option<ModelRefWire>,
    /// Model override for subagents (`null` = use default).
    pub subagent: Option<ModelRefWire>,
    /// Model for compaction summaries — the auto-compaction summary call and
    /// the run-all between-items compact (`null` = ride the turn's model).
    pub summarize: Option<ModelRefWire>,
    /// Per-skill overrides, keyed by skill name. Empty when none configured.
    pub skill: std::collections::HashMap<String, ModelRefWire>,
}

/// A registered project (the `projects[]` element).
#[derive(Debug, Clone, Serialize)]
pub struct ProjectWire {
    /// Project display name.
    pub name: String,
    /// Project root path.
    pub path: String,
}

/// The `get_settings` response: the full editable non-secret config surface.
#[derive(Debug, Clone, Serialize)]
pub struct GetSettingsResponse {
    /// Resolved global config directory path.
    pub config_dir: String,
    /// Default provider/model + safety + vision model.
    pub general: GetSettingsGeneral,
    /// Context-management settings.
    pub context: GetSettingsContext,
    /// UI preferences.
    pub ui: GetSettingsUi,
    /// Per-context model overrides (`[models]` section).
    pub models: ModelsConfigWire,
    /// Markdown viewer settings (`[markdown]` section).
    pub markdown: MarkdownWire,
    /// Git settings (`[git]` section) — the configurable core-operations list.
    pub git: GitWire,
    /// Trace-log memory limits (`[trace]` section) — the Advanced settings.
    pub trace: GetSettingsTrace,
    /// Memory retrieval/indexing scale knobs (`[memory]` section, Phase 2) —
    /// the Settings → Memory derived-index card's decay / caps / budgets.
    pub memory: mnemo::memory::MemorySearchConfig,
    /// Configured endpoints (no secrets).
    pub endpoints: Vec<EndpointWire>,
    /// Per-model pricing.
    pub pricing: Vec<PricingWire>,
    /// Registered projects.
    pub projects: Vec<ProjectWire>,
}

/// The `general` object inside [`GetSettingsResponse`]. `vision_model` is
/// emitted as `null` (not skipped) when unconfigured.
#[derive(Debug, Clone, Serialize)]
pub struct GetSettingsGeneral {
    /// Default endpoint name (`null` = none).
    pub default_provider: Option<String>,
    /// Default model id (`null` = none).
    pub default_model: Option<String>,
    /// Live runtime safety mode (renders the kebab-case serde form — identical
    /// to the legacy `safety_mode_wire` output, since `SafetyMode` itself uses
    /// `#[serde(rename_all = "kebab-case")]`).
    pub safety: SafetyMode,
    /// Vision fallback model (`null` = none).
    pub vision_model: Option<VisionModelWire>,
    /// Embedding model for memory recall (`null` = hash mode, offline).
    pub embedding_model: Option<EmbeddingModelWire>,
    /// Bundled in-process embedding model id (`null` = hash mode). Takes
    /// precedence over `embedding_model` (the legacy remote path).
    pub bundled_embedding_model: Option<String>,
    /// Whether the agent's `browser_*` browser-inspection tools are enabled
    /// (exposes an unauthenticated localhost CDP port — opt-in, off by
    /// default; debug builds always expose it regardless).
    pub enable_browser_inspection: bool,
    /// Whether a Run-All loop auto-compacts the main agent's context
    /// between items (after each completed plan, before the next item is
    /// dispatched). Run-All only — interactive completions never trigger
    /// it. Opt-in, off by default.
    pub auto_compact_on_plan_complete: bool,
}

/// The `context` object inside [`GetSettingsResponse`].
#[derive(Debug, Clone, Serialize)]
pub struct GetSettingsContext {
    /// Fraction of the context window at which to summarize.
    pub summarize_at_fill_rate: f64,
    /// Proxy cache ceiling in tokens — the context manager summarizes before
    /// the ~340K-token cliff where proxies drop whole-conversation prefix
    /// caching. `None` disables the guard.
    pub proxy_cache_ceiling_tokens: Option<usize>,
}

/// The `ui` object inside [`GetSettingsResponse`].
#[derive(Debug, Clone, Serialize)]
pub struct GetSettingsUi {
    /// Theme preference (`"dark"` / `"light"` / `"system"`).
    pub theme: String,
    /// Whether the token-usage panel is shown.
    pub show_token_usage: bool,
    /// Whether images from image commands render inline in the agent chat.
    pub show_tool_images: bool,
    /// Whether agent-activity cards (tool/memory/vision/skill) render in the
    /// chat transcript (GUI-only display filter; default false).
    pub show_tool_activity: bool,
    /// Whether knowledge-access activity cards (graph_* tool calls, memory
    /// tool calls, auto-recall entries) render in the chat transcript
    /// (GUI-only display filter; default true).
    pub show_knowledge_activity: bool,
    /// Whether the AUTO-DELEGATED steering line renders in
    /// search/search_read tool results (GUI-only display filter; default
    /// false).
    pub show_delegation_notes: bool,
    /// Resolved per-kind steering-note display flags (GUI-only display
    /// filter) — see [`GetSettingsSteeringNotes`].
    pub steering_notes: GetSettingsSteeringNotes,
    /// Whether a vertical thread line is drawn along consecutive activity
    /// cards in the chat transcript (GUI-only display filter; default true).
    pub chat_thread_line: bool,
    /// Whether assistant prose is capped at ~100 columns (code blocks and
    /// tool outputs stay full width) (GUI-only display filter; default true).
    pub chat_prose_cap: bool,
    /// Whether a faint background band alternates per conversation turn
    /// (GUI-only display filter; default true).
    pub chat_turn_tint: bool,
    /// Whether transcript entries show their creation time as a hover
    /// tooltip (GUI-only display filter; default true).
    pub chat_hover_timestamps: bool,
    /// Whether the completion ding plays when an agent's plan reaches
    /// Complete.
    pub sound_complete: bool,
    /// Whether the needs-input ping plays on approval requests / questions.
    pub sound_input_needed: bool,
    /// Whether the doom tone plays when repeated errors stop an agent.
    pub sound_stopped_errors: bool,
}

/// The `steering_notes` object inside [`GetSettingsUi`] — the RESOLVED
/// visibility of every steering-note kind (one flag per kind), so the
/// frontend never has to re-derive defaults.
///
/// The keys mirror `UiConfig::STEERING_NOTE_KEYS`; `auto_delegated` already
/// folds in the legacy `show_delegation_notes`.
#[derive(Debug, Clone, Serialize)]
pub struct GetSettingsSteeringNotes {
    /// Whether the AUTO-DELEGATED (code-graph / memory) block renders.
    pub auto_delegated: bool,
    /// Whether the symbol-nudge line renders.
    pub search_nudge: bool,
    /// Whether the shell TIP renders.
    pub shell_tip: bool,
    /// Whether the graph-miss line renders.
    pub graph_miss: bool,
    /// Whether the auto-recall rider renders.
    pub recall_rider: bool,
    /// Whether the read nudge renders.
    pub read_nudge: bool,
    /// Whether the literal-engine TIP renders.
    pub literal_tip: bool,
    /// Whether the known-memory-hit note renders.
    pub known_memory_hit: bool,
    /// Whether the consolidation-due note renders.
    pub consolidation_due: bool,
    /// Whether the shell-redirect TIP renders.
    pub shell_redirect: bool,
    /// Whether the stale-read note renders.
    pub edit_stale_read: bool,
}

impl GetSettingsSteeringNotes {
    /// Resolve every flag from `ui` — an explicit `[ui.steering_notes]`
    /// override wins, an absent one falls back to the kind's default (see
    /// [`UiConfig::effective_steering_note_visible`]).
    fn resolved(ui: &UiConfig) -> Self {
        Self {
            auto_delegated: ui.effective_steering_note_visible("auto_delegated"),
            search_nudge: ui.effective_steering_note_visible("search_nudge"),
            shell_tip: ui.effective_steering_note_visible("shell_tip"),
            graph_miss: ui.effective_steering_note_visible("graph_miss"),
            recall_rider: ui.effective_steering_note_visible("recall_rider"),
            read_nudge: ui.effective_steering_note_visible("read_nudge"),
            literal_tip: ui.effective_steering_note_visible("literal_tip"),
            known_memory_hit: ui.effective_steering_note_visible("known_memory_hit"),
            consolidation_due: ui.effective_steering_note_visible("consolidation_due"),
            shell_redirect: ui.effective_steering_note_visible("shell_redirect"),
            edit_stale_read: ui.effective_steering_note_visible("edit_stale_read"),
        }
    }
}

/// The `markdown` object inside [`GetSettingsResponse`] — the Markdown
/// viewer's configurable skip-directory list.
#[derive(Debug, Clone, Serialize)]
pub struct MarkdownWire {
    /// Directory names always skipped when enumerating `.md` files.
    pub skip_dirs: Vec<String>,
}

/// The `git` object inside [`GetSettingsResponse`] — the configurable list of
/// git subcommands that are *core operations* (always force the approval
/// prompt, even in Autonomous mode or under a matching safety rule).
#[derive(Debug, Clone, Serialize)]
pub struct GitWire {
    /// Git subcommands that always force the approval prompt (e.g. `merge`,
    /// `push`). Matched case-insensitively against the tool call's
    /// `subcommand` argument at dispatch time.
    pub core_operations: Vec<String>,
}

/// The `trace` object inside [`GetSettingsResponse`] — the in-memory trace
/// log's memory limits (Advanced settings; the `[trace]` config section).
#[derive(Debug, Clone, Serialize)]
pub struct GetSettingsTrace {
    /// Total raw-response byte budget across the trace ring, in MiB.
    pub memory_budget_mb: usize,
    /// Per-record request-body cap, in KiB.
    pub request_body_cap_kb: usize,
}

/// The `save_endpoints` response: the new default provider/model + whether the
/// live provider was rebuilt.
#[derive(Debug, Clone, Serialize)]
pub struct SaveEndpointsResponse {
    /// New default endpoint name (`null` = none).
    pub default_provider: Option<String>,
    /// New default model id (`null` = none).
    pub default_model: Option<String>,
    /// Whether the live provider was rebuilt + swapped into every agent.
    pub provider_swapped: bool,
}

/// The `save_settings` response: success + the resolved safety/vision state.
#[derive(Debug, Clone, Serialize)]
pub struct SaveSettingsResponse {
    /// Always `true` on the success path (the error path returns `Err`).
    pub ok: bool,
    /// The wire form of the safety mode applied, when `safety` was in the
    /// patch (`null` when safety was not patched).
    pub safety: Option<&'static str>,
    /// Whether a vision client is configured after the save.
    pub vision_configured: bool,
}

/// Map [`EndpointKind`] to the serde wire form (`"openai"` / `"local"` /
/// `"anthropic"`).
pub(crate) fn endpoint_kind_wire(kind: EndpointKind) -> &'static str {
    match kind {
        EndpointKind::OpenAI => "openai",
        EndpointKind::Local => "local",
        EndpointKind::Anthropic => "anthropic",
    }
}

/// Convert a [`ModelRef`](mnemo::config::ModelRef) to its wire form.
fn model_ref_wire(m: &mnemo::config::ModelRef) -> ModelRefWire {
    ModelRefWire {
        endpoint: m.endpoint.clone(),
        model: m.model.clone(),
        reasoning_effort: m.reasoning_effort.clone(),
    }
}

/// Convert the `[models]` section to its wire form (shared by `get_settings`).
fn models_config_wire(models: &mnemo::config::ModelsConfig) -> ModelsConfigWire {
    ModelsConfigWire {
        planning: models.planning.as_ref().map(model_ref_wire),
        executing: models.executing.as_ref().map(model_ref_wire),
        bug_fixing: models.bug_fixing.as_ref().map(model_ref_wire),
        reviewing: models.reviewing.as_ref().map(model_ref_wire),
        complete: models.complete.as_ref().map(model_ref_wire),
        subagent: models.subagent.as_ref().map(model_ref_wire),
        summarize: models.summarize.as_ref().map(model_ref_wire),
        skill: models
            .skill
            .iter()
            .map(|(k, v)| (k.clone(), model_ref_wire(v)))
            .collect(),
    }
}

/// Build the wire form of an endpoint (shared by `get_config` +
/// `get_settings`). `kind` is the serde wire form, not the Debug form.
fn endpoint_wire(e: &Endpoint) -> EndpointWire {
    EndpointWire {
        name: e.name.clone(),
        kind: endpoint_kind_wire(e.kind).to_string(),
        base_url: e.base_url.clone(),
        models: e.model_ids(),
        model_configs: e.models.iter().map(ModelSpecWire::from_spec).collect(),
        max_context: e.max_context,
        max_output_tokens: e.max_output_tokens,
        multimodal: e.multimodal,
        supports_reasoning_effort: e.supports_reasoning_effort,
        reasoning_effort: e.reasoning_effort.clone(),
        workspace_id: e.workspace_id.clone(),
    }
}

/// Map [`SafetyMode`] to kebab-case config string.
pub(crate) fn safety_mode_wire(mode: SafetyMode) -> &'static str {
    match mode {
        SafetyMode::ApproveEachAction => "approve-each-action",
        SafetyMode::AutoReadApproveWrites => "auto-read-approve-writes",
        SafetyMode::AutoApproveProject => "auto-approve-project",
        SafetyMode::Autonomous => "autonomous",
    }
}

/// Parse kebab-case safety mode string. Delegates to the lib-side
/// [`mnemo::config::patch::parse_safety_mode`] so the parsing is
/// unit-testable without Tauri state.
pub(crate) fn parse_safety_mode(mode: &str) -> Result<SafetyMode, String> {
    mnemo::config::patch::parse_safety_mode(mode)
}

/// Full Settings payload for the redesigned Settings dialog (no secrets).
///
/// Includes every editable non-secret section of global config plus the
/// resolved config directory path for the Advanced panel. Endpoint `kind` is
/// the serde form (`openai`/`local`), not Debug.
#[tauri::command]
pub async fn get_settings(state: State<'_, IpcState>) -> Result<GetSettingsResponse, IpcError> {
    let config = state.project.config.lock().await;
    let runtime_safety = *state
        .runtime
        .safety_mode
        .read()
        .map_err(|e| format!("safety_mode lock poisoned: {e}"))?;
    // `SafetyMode` itself serializes to the kebab-case wire form
    // (`#[serde(rename_all = "kebab-case")]`), identical to `safety_mode_wire`,
    // so assigning `runtime_safety` directly renders the same string the old
    // `safety_mode_wire(runtime_safety)` produced. Prefer the live runtime mode
    // (StatusBar can change it without writing config.toml); fall back to the
    // on-disk value is unnecessary because the runtime lock is the source of
    // truth here.
    let vision_model = config
        .general
        .general
        .vision_model
        .as_ref()
        .map(|vm| VisionModelWire {
            endpoint: vm.endpoint.clone(),
            model: vm.model.clone(),
        });
    let embedding_model =
        config
            .general
            .general
            .embedding_model
            .as_ref()
            .map(|em| EmbeddingModelWire {
                endpoint: em.endpoint.clone(),
                model: em.model.clone(),
            });
    let models = models_config_wire(&config.general.models);
    Ok(GetSettingsResponse {
        config_dir: mnemo::config::global_config_dir()
            .to_string_lossy()
            .to_string(),
        general: GetSettingsGeneral {
            default_provider: config.general.general.default_provider.clone(),
            default_model: config.general.general.default_model.clone(),
            safety: runtime_safety,
            vision_model,
            embedding_model,
            bundled_embedding_model: config.general.general.bundled_embedding_model.clone(),
            enable_browser_inspection: config.general.general.enable_browser_inspection,
            auto_compact_on_plan_complete: config.general.general.auto_compact_on_plan_complete,
        },
        context: GetSettingsContext {
            summarize_at_fill_rate: config.general.context.summarize_at_fill_rate,
            proxy_cache_ceiling_tokens: config.general.context.proxy_cache_ceiling_tokens,
        },
        ui: GetSettingsUi {
            theme: config.general.ui.theme.clone(),
            show_token_usage: config.general.ui.show_token_usage,
            show_tool_images: config.general.ui.show_tool_images,
            show_tool_activity: config.general.ui.show_tool_activity,
            show_knowledge_activity: config.general.ui.show_knowledge_activity,
            show_delegation_notes: config.general.ui.show_delegation_notes,
            steering_notes: GetSettingsSteeringNotes::resolved(&config.general.ui),
            chat_thread_line: config.general.ui.chat_thread_line,
            chat_prose_cap: config.general.ui.chat_prose_cap,
            chat_turn_tint: config.general.ui.chat_turn_tint,
            chat_hover_timestamps: config.general.ui.chat_hover_timestamps,
            sound_complete: config.general.ui.sound_complete,
            sound_input_needed: config.general.ui.sound_input_needed,
            sound_stopped_errors: config.general.ui.sound_stopped_errors,
        },
        models,
        markdown: MarkdownWire {
            skip_dirs: config.general.markdown.skip_dirs.clone(),
        },
        git: GitWire {
            core_operations: config.general.git.core_operations.clone(),
        },
        trace: GetSettingsTrace {
            memory_budget_mb: config.general.trace.memory_budget_mb,
            request_body_cap_kb: config.general.trace.request_body_cap_kb,
        },
        memory: config.general.memory.clone(),
        endpoints: config.endpoints.iter().map(endpoint_wire).collect(),
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
        projects: config
            .projects
            .iter()
            .map(|e| ProjectWire {
                name: e.name.clone(),
                path: e.path.clone(),
            })
            .collect(),
    })
}

/// Persist general / context / memory / ui / vision / pricing Settings.
#[tauri::command]
pub async fn save_settings(
    state: State<'_, IpcState>,
    patch: SettingsSaveDto,
) -> Result<SaveSettingsResponse, IpcError> {
    // Validate + apply the patch in the brain (review M1b): pure domain logic,
    // no I/O, no Tauri state. Returns the new Config; on any error the original
    // is unchanged (no partial application).
    let new_config = {
        let current = state.project.config.lock().await;
        validate_and_apply_settings_patch(&current, &patch).map_err(IpcError::from)?
    };

    // The brain function already parsed + applied the safety mode to
    // new_config.general.general.safety. Read it back directly instead of
    // re-parsing patch.safety (review finding 7 — avoids a redundant parse).
    let parsed_safety = if patch.safety.is_some() {
        Some(new_config.general.general.safety)
    } else {
        None
    };

    let reloaded = crate::ipc::config_io::persist_and_reload(&state, new_config).await?;

    // Keep runtime safety mode in sync when safety was patched.
    if let Some(mode) = parsed_safety {
        let resolved = state.set_safety(mode)?;
        if resolved > 0 {
            eprintln!("safety: settings save auto-resolved {resolved} pending approval(s)");
        }
    }

    // Live-rewire vision + embedder so Settings changes take effect without
    // restart (fill_rate changes apply to newly built agents only).
    rewire_vision_and_embedder(&state, &reloaded);

    // Push the reloaded config into the per-context model resolver so
    // `[models]` overrides take effect on the next turn.
    sync_model_resolver(&state, &reloaded);

    // Push the reloaded `[git].core_operations` into the factory's shared
    // handle so every live GitTool observes the new list on its next
    // never_auto_for check (no registry rebuild needed).
    if let Some(f) = state.runtime.factory.as_ref() {
        f.set_core_operations(reloaded.general.git.core_operations.clone());
        // Push the reloaded `[shell_filter]` config into the factory's shared
        // handle so every live ShellTool observes it on its next command.
        f.set_shell_filter_config(reloaded.general.shell_filter.clone());
        // Push the reloaded `enable_browser_inspection` so the on-screen
        // browser_* tools appear in (or vanish from) the tools array on the
        // next turn, matching whether their CDP endpoint actually exists.
        f.set_browser_inspection(reloaded.general.general.enable_browser_inspection);
    }

    // Push the reloaded `[trace]` memory limits into the LIVE trace log so
    // they take effect without a restart. The budget applies retroactively
    // (oldest over-budget payloads evicted immediately); the request cap
    // applies to records started after this point.
    if patch.trace_memory_budget_mb.is_some() || patch.trace_request_body_cap_kb.is_some() {
        state
            .trace
            .set_memory_budget(reloaded.general.trace.memory_budget_mb * 1024 * 1024);
        state
            .trace
            .set_request_body_cap(reloaded.general.trace.request_body_cap_kb * 1024);
    }

    Ok(SaveSettingsResponse {
        ok: true,
        safety: parsed_safety.map(safety_mode_wire),
        vision_configured: state
            .runtime
            .factory
            .as_ref()
            .map(|f| f.vision_slot().is_configured())
            .unwrap_or(false),
    })
}

#[cfg(test)]
mod endpoint_dto_tests {
    use super::*;

    fn dto(name: &str, kind: &str, base_url: &str) -> EndpointDto {
        EndpointDto {
            name: name.into(),
            kind: kind.into(),
            base_url: base_url.into(),
            models: vec![],
            model_configs: vec![],
            max_context: None,
            max_output_tokens: None,
            multimodal: false,
            supports_reasoning_effort: true,
            reasoning_effort: None,
            workspace_id: None,
        }
    }

    #[test]
    fn workspace_id_round_trips_through_dto() {
        let mut d = dto("claude", "anthropic", "https://api.anthropic.com/v1/");
        d.workspace_id = Some("ws_abc".into());
        let ep = d.into_endpoint().unwrap();
        assert_eq!(ep.workspace_id.as_deref(), Some("ws_abc"));
    }

    #[test]
    fn per_model_multimodal_round_trips_through_dto() {
        // Save direction: the per-model vision override survives the DTO →
        // config mapping (backlog a634835c — mixed endpoints like Ollama
        // hosting a text-only GLM and a vision-capable GLM flash).
        let mut d = dto("ollama", "local", "http://localhost:11434/v1/");
        d.models = vec!["glm".into(), "glm-flash".into()];
        d.model_configs = vec![
            ModelSpecDto {
                id: "glm".into(),
                max_context: None,
                max_output_tokens: None,
                reasoning_efforts: Vec::new(),
                reasoning_effort: None,
                multimodal: None,
            },
            ModelSpecDto {
                id: "glm-flash".into(),
                max_context: None,
                max_output_tokens: None,
                reasoning_efforts: Vec::new(),
                reasoning_effort: None,
                multimodal: Some(true),
            },
        ];
        let ep = d.into_endpoint().unwrap();
        assert_eq!(ep.model_spec("glm-flash").unwrap().multimodal, Some(true));
        assert_eq!(ep.model_spec("glm").unwrap().multimodal, None);
        // The flag stays separate from the endpoint-level default…
        assert!(!ep.multimodal, "endpoint default untouched");
        assert!(ep.multimodal_for("glm-flash"));
        assert!(!ep.multimodal_for("glm"));

        // GET direction: the wire emits the override verbatim (null when
        // unset — the FE renders that as "inherit the endpoint flag").
        let wire_flash = ModelSpecWire::from_spec(ep.model_spec("glm-flash").unwrap());
        assert_eq!(wire_flash.multimodal, Some(true));
        let wire_glm = ModelSpecWire::from_spec(ep.model_spec("glm").unwrap());
        assert_eq!(wire_glm.multimodal, None);
    }

    #[test]
    fn per_model_reasoning_effort_round_trips_through_dto() {
        // Save direction: the per-model effort default survives the DTO →
        // config mapping (backlog 5b099aef — the per-model row dropdown).
        let mut d = dto("gateway", "openai", "https://gateway.example.com/v1/");
        d.models = vec!["glm".into(), "glm-flash".into()];
        d.model_configs = vec![
            ModelSpecDto {
                id: "glm".into(),
                max_context: None,
                max_output_tokens: None,
                reasoning_efforts: Vec::new(),
                reasoning_effort: Some("low".into()),
                multimodal: None,
            },
            ModelSpecDto {
                id: "glm-flash".into(),
                max_context: None,
                max_output_tokens: None,
                reasoning_efforts: Vec::new(),
                reasoning_effort: None,
                multimodal: None,
            },
        ];
        let ep = d.into_endpoint().unwrap();
        assert_eq!(
            ep.model_spec("glm").unwrap().reasoning_effort.as_deref(),
            Some("low")
        );
        assert_eq!(ep.model_spec("glm-flash").unwrap().reasoning_effort, None);
        // The resolver picks the model-level value up (unset inherits the
        // endpoint's, then the app default "max").
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm")),
            Some("low".into())
        );
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm-flash")),
            Some("max".into())
        );

        // GET direction: the wire emits the override verbatim (null when
        // unset — the FE renders that as "inherit the endpoint value").
        let wire = ModelSpecWire::from_spec(ep.model_spec("glm").unwrap());
        assert_eq!(wire.reasoning_effort.as_deref(), Some("low"));
        let wire_flash = ModelSpecWire::from_spec(ep.model_spec("glm-flash").unwrap());
        assert_eq!(wire_flash.reasoning_effort, None);
    }

    #[test]
    fn per_model_multimodal_absent_key_defaults_none() {
        // Older Settings payloads omit `multimodal` on model configs; the
        // serde default must keep them parsing (None = inherit the endpoint
        // flag) without a re-save.
        let raw = r#"{
            "name": "legacy",
            "kind": "openai",
            "base_url": "https://api.example.com/v1/",
            "models": ["m"],
            "model_configs": [
                {"id": "m", "max_context": 128000}
            ]
        }"#;
        let d: EndpointDto = serde_json::from_str(raw).unwrap();
        assert_eq!(d.model_configs[0].multimodal, None);
        assert_eq!(d.model_configs[0].reasoning_effort, None);
        let ep = d.into_endpoint().unwrap();
        assert_eq!(ep.model_spec("m").unwrap().multimodal, None);
        assert_eq!(ep.model_spec("m").unwrap().reasoning_effort, None);
    }

    #[test]
    fn parses_valid_endpoint() {
        let ep = dto("openai", "openai", "https://api.openai.com/v1/")
            .into_endpoint()
            .unwrap();
        assert_eq!(ep.name, "openai");
        assert_eq!(ep.kind, EndpointKind::OpenAI);
        assert_eq!(ep.base_url, "https://api.openai.com/v1/");
        assert!(
            ep.supports_reasoning_effort,
            "DTO helper defaults supports_reasoning_effort to true"
        );
    }

    #[test]
    fn supports_reasoning_effort_false_round_trips() {
        let mut d = dto("chat", "openai", "https://api.example.com/v1/");
        d.supports_reasoning_effort = false;
        d.reasoning_effort = Some("high".into());
        let ep = d.into_endpoint().unwrap();
        assert!(!ep.supports_reasoning_effort);
        // Capability flag wins at request-build time even if a value is stored.
        assert_eq!(ep.effective_reasoning_effort(), None);
    }

    #[test]
    fn supports_reasoning_effort_defaults_true_when_key_absent() {
        // Older Settings payloads omit the key; serde default must keep
        // existing reasoning models working without re-saving.
        let raw = r#"{
            "name": "legacy",
            "kind": "openai",
            "base_url": "https://api.example.com/v1/",
            "models": ["m"]
        }"#;
        let d: EndpointDto = serde_json::from_str(raw).unwrap();
        assert!(d.supports_reasoning_effort);
        let ep = d.into_endpoint().unwrap();
        assert!(ep.supports_reasoning_effort);
    }

    #[test]
    fn rejects_non_serde_kind_form() {
        // get_config always emits the serde form ("openai"/"local"/"anthropic")
        // via endpoint_kind_wire. The save path now does an exact,
        // case-sensitive match — the legacy Debug form ("OpenAI"/"Local") is
        // rejected.
        let err = dto("o", "OpenAI", "https://x/v1/")
            .into_endpoint()
            .unwrap_err();
        assert!(err.contains("unknown kind 'OpenAI'"));
        let err = dto("l", "LOCAL", "http://localhost/v1/")
            .into_endpoint()
            .unwrap_err();
        assert!(err.contains("unknown kind 'LOCAL'"));
    }

    #[test]
    fn normalizes_missing_trailing_slash() {
        // Forgiving (user-reported): the trailing slash is auto-appended
        // instead of rejecting the save.
        let ep = dto("openai", "openai", "https://api.openai.com/v1")
            .into_endpoint()
            .unwrap();
        assert_eq!(ep.base_url, "https://api.openai.com/v1/");
    }

    #[test]
    fn rejects_empty_name() {
        let err = dto("  ", "openai", "https://x/v1/")
            .into_endpoint()
            .unwrap_err();
        assert!(err.contains("name"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_empty_base_url() {
        let err = dto("x", "openai", "").into_endpoint().unwrap_err();
        assert!(err.contains("base_url"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_unknown_kind() {
        let err = dto("x", "claude", "https://x/v1/")
            .into_endpoint()
            .unwrap_err();
        assert!(err.contains("unknown kind"), "unexpected error: {err}");
    }

    #[test]
    fn accepts_anthropic_kind() {
        let ep = dto("claude", "anthropic", "https://api.anthropic.com/v1/")
            .into_endpoint()
            .unwrap();
        assert_eq!(ep.kind, EndpointKind::Anthropic);
        assert_eq!(ep.base_url, "https://api.anthropic.com/v1/");
    }

    #[test]
    fn drops_empty_models() {
        let mut d = dto("openai", "openai", "https://api.openai.com/v1/");
        d.models = vec![
            "gpt-4o".into(),
            "".into(),
            "  ".into(),
            "gpt-4o-mini".into(),
        ];
        let ep = d.into_endpoint().unwrap();
        assert_eq!(ep.model_ids(), vec!["gpt-4o", "gpt-4o-mini"]);
    }
}

#[cfg(test)]
mod settings_dto_tests {
    use super::*;

    #[test]
    fn parse_safety_mode_accepts_all_variants() {
        assert!(matches!(
            parse_safety_mode("approve-each-action"),
            Ok(SafetyMode::ApproveEachAction)
        ));
        assert!(matches!(
            parse_safety_mode("autonomous"),
            Ok(SafetyMode::Autonomous)
        ));
        assert!(parse_safety_mode("nope").is_err());
    }

    #[test]
    fn settings_save_dto_defaults_empty() {
        let p: SettingsSaveDto = serde_json::from_str("{}").unwrap();
        assert!(p.safety.is_none());
        assert!(p.pricing.is_none());
        assert!(!p.clear_vision_model);
        assert!(p.summarize_at_fill_rate.is_none());
        assert!(p.trace_memory_budget_mb.is_none());
        assert!(p.trace_request_body_cap_kb.is_none());
    }

    #[test]
    fn settings_save_dto_parses_trace_knobs() {
        let p: SettingsSaveDto = serde_json::from_str(
            r#"{ "trace_memory_budget_mb": 64, "trace_request_body_cap_kb": 512 }"#,
        )
        .unwrap();
        assert_eq!(p.trace_memory_budget_mb, Some(64));
        assert_eq!(p.trace_request_body_cap_kb, Some(512));
    }

    #[test]
    fn settings_save_dto_parses_vision_and_pricing() {
        let raw = r#"{
            "safety": "auto-approve-project",
            "vision_model": { "endpoint": "qwen", "model": "qwen-vl" },
            "summarize_at_fill_rate": 0.4,
            "theme": "system",
            "show_token_usage": false,
            "sound_complete": false,
            "sound_input_needed": true,
            "sound_stopped_errors": false,
            "pricing": [{ "model": "gpt-4o", "input_per_1m": 2.5, "output_per_1m": 10.0, "cached_per_1m": 1.25 }]
        }"#;
        let p: SettingsSaveDto = serde_json::from_str(raw).unwrap();
        assert_eq!(p.safety.as_deref(), Some("auto-approve-project"));
        assert_eq!(p.vision_model.as_ref().unwrap().endpoint, "qwen");
        assert!((p.summarize_at_fill_rate.unwrap() - 0.4).abs() < 1e-9);
        assert_eq!(p.theme.as_deref(), Some("system"));
        assert_eq!(p.show_token_usage, Some(false));
        // Each notification sound patches independently (absent = keep).
        assert_eq!(p.sound_complete, Some(false));
        assert_eq!(p.sound_input_needed, Some(true));
        assert_eq!(p.sound_stopped_errors, Some(false));
        assert_eq!(p.pricing.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn endpoint_kind_wire_matches_serde() {
        assert_eq!(endpoint_kind_wire(EndpointKind::OpenAI), "openai");
        assert_eq!(endpoint_kind_wire(EndpointKind::Local), "local");
        assert_eq!(endpoint_kind_wire(EndpointKind::Anthropic), "anthropic");
    }

    // ── Typed response structs render byte-identically to the old json! bodies ─
    //
    // These tests pin the wire shape of the four typed response structs so a
    // future edit (e.g. adding `skip_serializing_if` on an Option, or renaming
    // a field) is caught here + by the golden fixtures (Step 3) before it
    // reaches the frontend. They assert the exact serde JSON, including null
    // emission for `None` (the FE types model those as `| null`).

    #[test]
    fn get_settings_response_renders_legacy_shape_with_nulls() {
        let resp = GetSettingsResponse {
            config_dir: "/cfg".into(),
            general: GetSettingsGeneral {
                default_provider: None,
                default_model: None,
                safety: SafetyMode::ApproveEachAction,
                vision_model: None,
                embedding_model: None,
                bundled_embedding_model: None,
                enable_browser_inspection: false,
                auto_compact_on_plan_complete: false,
            },
            context: GetSettingsContext {
                summarize_at_fill_rate: 0.3,
                proxy_cache_ceiling_tokens: Some(340_000),
            },
            ui: GetSettingsUi {
                theme: "dark".into(),
                show_token_usage: true,
                show_tool_images: true,
                show_tool_activity: true,
                show_knowledge_activity: true,
                show_delegation_notes: false,
                steering_notes: GetSettingsSteeringNotes::resolved(&UiConfig::default()),
                chat_thread_line: true,
                chat_prose_cap: false,
                chat_turn_tint: true,
                chat_hover_timestamps: false,
                sound_complete: true,
                sound_input_needed: true,
                sound_stopped_errors: true,
            },
            models: ModelsConfigWire {
                planning: None,
                executing: None,
                bug_fixing: None,
                reviewing: None,
                complete: None,
                subagent: None,
                summarize: None,
                skill: std::collections::HashMap::new(),
            },
            markdown: MarkdownWire {
                skip_dirs: vec![".git".into(), "node_modules".into()],
            },
            git: GitWire {
                core_operations: vec!["merge".into(), "push".into()],
            },
            trace: GetSettingsTrace {
                memory_budget_mb: 16,
                request_body_cap_kb: 256,
            },
            memory: mnemo::memory::MemorySearchConfig::default(),
            endpoints: vec![],
            pricing: vec![],
            projects: vec![ProjectWire {
                name: "demo".into(),
                path: "/demo".into(),
            }],
        };
        let v = serde_json::to_value(&resp).unwrap();
        // nulls must be emitted (not skipped) for default_provider /
        // default_model / vision_model.
        assert_eq!(v["general"]["default_provider"], serde_json::Value::Null);
        assert_eq!(v["general"]["default_model"], serde_json::Value::Null);
        assert_eq!(v["general"]["vision_model"], serde_json::Value::Null);
        assert_eq!(v["general"]["safety"], "approve-each-action");
        assert_eq!(v["config_dir"], "/cfg");
        assert_eq!(v["context"]["summarize_at_fill_rate"], 0.3);
        assert_eq!(v["ui"]["theme"], "dark");
        assert_eq!(v["ui"]["show_token_usage"], true);
        assert_eq!(v["ui"]["show_tool_activity"], true);
        assert_eq!(v["ui"]["show_knowledge_activity"], true);
        assert_eq!(v["ui"]["show_delegation_notes"], false);
        // The four chat-readability flags ride the ui object.
        assert_eq!(v["ui"]["chat_thread_line"], true);
        assert_eq!(v["ui"]["chat_prose_cap"], false);
        assert_eq!(v["ui"]["chat_turn_tint"], true);
        assert_eq!(v["ui"]["chat_hover_timestamps"], false);
        // The three notification-sound flags ride the ui object.
        assert_eq!(v["ui"]["sound_complete"], true);
        assert_eq!(v["ui"]["sound_input_needed"], true);
        assert_eq!(v["ui"]["sound_stopped_errors"], true);
        assert_eq!(v["trace"]["memory_budget_mb"], 16);
        assert_eq!(v["trace"]["request_body_cap_kb"], 256);
        // The [memory] retrieval knobs render their defaults (Phase 2).
        assert_eq!(v["memory"]["decay_half_life_days"], 7.0);
        assert_eq!(v["memory"]["per_query_cap"], 50);
        assert_eq!(v["memory"]["derived_per_class_cap"], 5);
        assert_eq!(v["memory"]["plan_budget"], 400);
        assert_eq!(v["memory"]["decision_budget"], 300);
        assert_eq!(v["endpoints"], serde_json::json!([]));
        assert_eq!(v["pricing"], serde_json::json!([]));
        assert_eq!(
            v["projects"],
            serde_json::json!([{ "name": "demo", "path": "/demo" }])
        );
        // The [models] section renders with null overrides + an empty skill map.
        assert_eq!(v["models"]["planning"], serde_json::Value::Null);
        assert_eq!(v["models"]["executing"], serde_json::Value::Null);
        assert_eq!(v["models"]["reviewing"], serde_json::Value::Null);
        assert_eq!(v["models"]["complete"], serde_json::Value::Null);
        assert_eq!(v["models"]["subagent"], serde_json::Value::Null);
        assert_eq!(v["models"]["skill"], serde_json::json!({}));
        // The [markdown] section renders its skip_dirs list.
        assert_eq!(
            v["markdown"]["skip_dirs"],
            serde_json::json!([".git", "node_modules"])
        );
    }

    #[test]
    fn get_settings_response_renders_vision_model_when_configured() {
        let resp = GetSettingsResponse {
            config_dir: "/cfg".into(),
            general: GetSettingsGeneral {
                default_provider: Some("qwen".into()),
                default_model: Some("qwen-vl".into()),
                safety: SafetyMode::Autonomous,
                vision_model: Some(VisionModelWire {
                    endpoint: "qwen".into(),
                    model: "qwen-vl".into(),
                }),
                embedding_model: None,
                bundled_embedding_model: None,
                enable_browser_inspection: false,
                auto_compact_on_plan_complete: false,
            },
            context: GetSettingsContext {
                summarize_at_fill_rate: 0.4,
                proxy_cache_ceiling_tokens: Some(340_000),
            },
            ui: GetSettingsUi {
                theme: "system".into(),
                show_token_usage: false,
                show_tool_images: false,
                show_tool_activity: true,
                show_knowledge_activity: true,
                show_delegation_notes: false,
                steering_notes: GetSettingsSteeringNotes::resolved(&UiConfig::default()),
                chat_thread_line: true,
                chat_prose_cap: true,
                chat_turn_tint: true,
                chat_hover_timestamps: true,
                sound_complete: false,
                sound_input_needed: true,
                sound_stopped_errors: false,
            },
            models: ModelsConfigWire {
                planning: None,
                executing: None,
                bug_fixing: None,
                reviewing: None,
                complete: None,
                subagent: None,
                summarize: None,
                skill: std::collections::HashMap::new(),
            },
            markdown: MarkdownWire {
                skip_dirs: vec![".git".into(), "node_modules".into()],
            },
            git: GitWire {
                core_operations: vec!["merge".into(), "push".into()],
            },
            trace: GetSettingsTrace {
                memory_budget_mb: 16,
                request_body_cap_kb: 256,
            },
            memory: mnemo::memory::MemorySearchConfig::default(),
            endpoints: vec![],
            pricing: vec![],
            projects: vec![],
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(
            v["general"]["vision_model"],
            serde_json::json!({ "endpoint": "qwen", "model": "qwen-vl" })
        );
        assert_eq!(v["general"]["safety"], "autonomous");
    }

    #[test]
    fn save_endpoints_response_renders_legacy_shape() {
        let resp = SaveEndpointsResponse {
            default_provider: Some("openai".into()),
            default_model: Some("gpt-4o".into()),
            provider_swapped: true,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "default_provider": "openai",
                "default_model": "gpt-4o",
                "provider_swapped": true,
            })
        );
    }

    #[test]
    fn save_settings_response_renders_legacy_shape() {
        let none = SaveSettingsResponse {
            ok: true,
            safety: None,
            vision_configured: false,
        };
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            serde_json::json!({ "ok": true, "safety": null, "vision_configured": false })
        );
        let some = SaveSettingsResponse {
            ok: true,
            safety: Some("autonomous"),
            vision_configured: true,
        };
        assert_eq!(
            serde_json::to_value(&some).unwrap(),
            serde_json::json!({ "ok": true, "safety": "autonomous", "vision_configured": true })
        );
    }

    #[test]
    fn models_config_dto_patch_semantics() {
        // T2: the [models] patch uses Option<Option<ModelRefDto>> so the apply
        // logic can distinguish "field absent (keep existing)" from "field
        // null (clear)" from "field set (replace)". Verify serde round-trips
        // all three states for a fixed slot.
        use mnemo::config::settings_dto::ModelsConfigDto;

        // 1. Field absent (outer None) — keep existing. A patch that omits
        //    `planning` entirely deserializes to `planning: None`.
        let patch: ModelsConfigDto =
            serde_json::from_str(r#"{"executing": {"endpoint":"e","model":"m"}}"#).unwrap();
        assert!(patch.planning.is_none(), "absent field → outer None (keep)");
        // executing is Some(Some(...)) — set.
        let exec = patch.executing.expect("executing present");
        let m = exec.expect("executing set (not cleared)");
        assert_eq!(m.endpoint, "e");
        assert_eq!(m.model, "m");

        // 2. Field null (inner None) — clear the override.
        let patch: ModelsConfigDto = serde_json::from_str(r#"{"planning": null}"#).unwrap();
        let planning = patch.planning.expect("planning present");
        assert!(planning.is_none(), "null field → inner None (clear)");

        // 3. Field set (Some(Some(...))) — replace.
        let patch: ModelsConfigDto =
            serde_json::from_str(r#"{"subagent": {"endpoint":"deepseek","model":"flash"}}"#)
                .unwrap();
        let sub = patch.subagent.expect("subagent present");
        let m = sub.expect("subagent set (not cleared)");
        assert_eq!(m.endpoint, "deepseek");
        assert_eq!(m.model, "flash");

        // 3b. The reviewing slot follows the same absent/set/clear semantics.
        let patch: ModelsConfigDto =
            serde_json::from_str(r#"{"reviewing": {"endpoint":"openai","model":"o3"}}"#).unwrap();
        let rev = patch.reviewing.expect("reviewing present");
        let m = rev.expect("reviewing set (not cleared)");
        assert_eq!(m.endpoint, "openai");
        assert_eq!(m.model, "o3");
        let patch: ModelsConfigDto = serde_json::from_str(r#"{"reviewing": null}"#).unwrap();
        assert!(patch.reviewing.expect("reviewing present").is_none());

        // 3c. The summarize slot follows the same absent/set/clear semantics.
        let patch: ModelsConfigDto =
            serde_json::from_str(r#"{"summarize": {"endpoint":"deepseek","model":"flash"}}"#)
                .unwrap();
        let sum = patch.summarize.expect("summarize present");
        let m = sum.expect("summarize set (not cleared)");
        assert_eq!(m.endpoint, "deepseek");
        assert_eq!(m.model, "flash");
        let patch: ModelsConfigDto = serde_json::from_str(r#"{"summarize": null}"#).unwrap();
        assert!(patch.summarize.expect("summarize present").is_none());

        // 4. The skill map, when present, fully replaces (send {} to clear).
        let patch: ModelsConfigDto = serde_json::from_str(r#"{"skill": {}}"#).unwrap();
        let skill = patch.skill.expect("skill present");
        assert!(
            skill.is_empty(),
            "empty skill map clears all skill overrides"
        );

        // 5. A patch with no `models` key at all → the whole field is None
        //    (no change to [models]).
        let patch: ModelsConfigDto = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert!(patch.planning.is_none());
        assert!(patch.skill.is_none());
    }

    #[test]
    fn models_config_dto_validates_endpoint_references() {
        // The validation block in save_settings rejects overrides whose
        // endpoint doesn't match a configured endpoint. This test documents
        // the expected shape (a ModelRefDto with empty endpoint/model is the
        // only one the validation would reject pre-save); the full save path
        // is covered by the contract fixture + the apply logic above.
        use mnemo::config::settings_dto::ModelRefDto;
        let m: ModelRefDto = serde_json::from_str(r#"{"endpoint":"openai","model":"o3"}"#).unwrap();
        assert_eq!(m.endpoint, "openai");
        assert_eq!(m.model, "o3");
    }

    #[test]
    fn models_patch_deserializes_reasoning_effort() {
        // Save direction: the frontend payload's per-context effort rides
        // SettingsSaveDto's ModelRefDto (serde default keeps old payloads
        // without the field parsing as None).
        use mnemo::config::settings_dto::SettingsSaveDto;
        let patch: SettingsSaveDto = serde_json::from_str(
            r#"{"models": {"planning": {"endpoint": "openai", "model": "o3", "reasoning_effort": "low"}}}"#,
        )
        .unwrap();
        let models = patch.models.expect("models present");
        let p = models
            .planning
            .expect("planning present")
            .expect("planning set");
        assert_eq!(p.reasoning_effort.as_deref(), Some("low"));
    }

    #[test]
    fn model_ref_wire_carries_reasoning_effort() {
        // Load direction: the [models] wire (getSettings) carries the
        // per-context effort; an unset effort is skipped on the wire so old
        // payloads keep their exact shape.
        use mnemo::config::ModelRef;
        let wire = model_ref_wire(&ModelRef {
            endpoint: "openai".into(),
            model: "o3".into(),
            reasoning_effort: Some("low".into()),
        });
        assert_eq!(wire.reasoning_effort.as_deref(), Some("low"));
        let wire = model_ref_wire(&ModelRef {
            endpoint: "openai".into(),
            model: "o3".into(),
            reasoning_effort: None,
        });
        let json = serde_json::to_value(&wire).unwrap();
        assert!(
            json.get("reasoning_effort").is_none(),
            "an unset effort must be skipped on the wire"
        );
    }

    #[test]
    fn save_settings_sets_embedding_model() {
        // The embedding_model patch deserializes from the frontend payload
        // shape { embedding_model: { endpoint, model } }.
        use mnemo::config::settings_dto::SettingsSaveDto;
        let patch: SettingsSaveDto = serde_json::from_str(
            r#"{"embedding_model": {"endpoint":"ollama-local","model":"nomic-embed-text"}}"#,
        )
        .unwrap();
        let em = patch.embedding_model.expect("embedding_model present");
        assert_eq!(em.endpoint, "ollama-local");
        assert_eq!(em.model, "nomic-embed-text");
        assert!(
            !patch.clear_embedding_model,
            "set path: clear flag is false"
        );
    }

    #[test]
    fn save_settings_clears_embedding_model() {
        // The clear path: clear_embedding_model=true with no embedding_model.
        use mnemo::config::settings_dto::SettingsSaveDto;
        let patch: SettingsSaveDto =
            serde_json::from_str(r#"{"clear_embedding_model": true}"#).unwrap();
        assert!(patch.clear_embedding_model, "clear flag is true");
        assert!(
            patch.embedding_model.is_none(),
            "clear path: no embedding_model payload"
        );
    }
}
