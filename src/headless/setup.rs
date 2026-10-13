// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The standalone setup + config API (§5.8 :486-589, U8) — everything the
//! GUI Settings, the project picker, and the first-run screens do,
//! reachable from code so a standalone install has no GUI dependency.
//!
//! Job 4 lands it in steps: D1 (this module) carries the readiness
//! report — [`Setup::open`], [`Setup::status`] (the [`SetupStatus`]
//! snapshot + the ordered [`SetupStep`] to-do list) and the
//! [`Engine::start`](crate::headless::Engine::start) precondition (an
//! unready config fails fast with error code `not_configured` instead
//! of deep inside the brain; a missing embedding model is NOT fatal —
//! the engine runs on the deterministic hash fallback and `semantic:
//! false` reports it). The mutating surface (endpoints + keys D2,
//! settings D4, the classifier D5, the embeddings D6, the project group
//! D8) threads through this type.
//!
//! G4 (§5.9.4 :676-677): this module never CALLS `global_config_dir` —
//! the dir arrives explicitly (or via
//! [`crate::config::resolve_config_dir`], which owns the `$MNEMO_HOME`
//! default) and every derivation helper lives in `config/`.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::keys::{KeySource, Secret};
use crate::config::mcp::McpServerWire;
use crate::config::settings_dto::{SettingsSaveDto, VisionModelDto};

/// Which agent.md a Setup method addresses (§5.8 :557): the install's
/// GLOBAL one (under the config dir) or a PROJECT's root one.
#[derive(Debug, Clone, PartialEq)]
pub enum Scope {
    /// `<config_dir>/agent.md` — the install-global constitution.
    Global,
    /// `<root>/agent.md` — the project's constitution.
    Project(std::path::PathBuf),
}

/// §5.8 :556's options: whether [`Setup::init_project`] also registers
/// the project in `projects.toml`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InitProjectOptions {
    /// Add the project to the registry (`<config_dir>/projects.toml`).
    pub register: bool,
}

/// §5.8 :556's result: the scaffolded project's key paths + whether it
/// was registered.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectInfo {
    /// The project root.
    pub root: std::path::PathBuf,
    /// The `.coding/` side-car dir.
    pub coding_dir: std::path::PathBuf,
    /// The root `agent.md` path.
    pub agent_md: std::path::PathBuf,
    /// Was the project added to `projects.toml`?
    pub registered: bool,
}

/// §5.8 :559-560 — a project's safety rules (the `[[rule]]` set
/// `ApproveViaApi` consults at dispatch time), one entry per rule.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SafetyRulesDto {
    /// The rules, in file order.
    #[serde(default)]
    pub rules: Vec<SafetyRuleDto>,
}

/// One safety rule in the wire form (§5.8 :559).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SafetyRuleDto {
    /// The tool name (e.g. `"file_write"`).
    pub tool: String,
    /// The regex (literal rules) or the normalized safety class
    /// (command_class rules).
    pub pattern: String,
    /// How `pattern` is interpreted (defaults to `literal`).
    #[serde(default, skip_serializing_if = "crate::safety_rules::RuleKind::is_literal")]
    pub kind: crate::safety_rules::RuleKind,
}

/// §5.8 :552's result: the outcome of a Settings-style "Test" click —
/// connect to the named server and list its tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpTestResult {
    /// Whether connect + initialize + tools/list all succeeded.
    pub ok: bool,
    /// How many tools the server advertises (0 unless `ok`).
    pub tool_count: usize,
    /// The advertised tool names (empty unless `ok`).
    pub tool_names: Vec<String>,
    /// The failure detail (None on success).
    pub error: Option<String>,
}
use crate::config::{Config, EndpointKind, SafetyMode};
use crate::headless::error::EngineError;
use crate::headless::types::EventReceiver;
pub use crate::host::embed_download::EmbeddingModelDto;
#[cfg(feature = "embeddings")]
use crate::host::embed_download::EmbedderStatusSink;
use crate::host::laya::{LayaManager, StatusSink};
use crate::memory::classifier::{Classifier, ClassifierStatus};
use crate::memory::embedder::EmbedderStatus;

/// One to-do of a first-run flow (§5.8 :570) — the ordered list a setup
/// UI walks the user through. [`SetupStep::DefaultModel`] is the only
/// FATAL leg (it gates `Engine::start` for a caller without an injected
/// provider); the others are opt-in conveniences the engine runs
/// without (hash-fallback embeddings, an unscaffolded workspace).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStep {
    /// Configure a usable default model: at least one endpoint (with a
    /// key, when its kind needs one) and `[general] default_model`
    /// served by it.
    DefaultModel,
    /// Install the configured bundled embedding model — optional: the
    /// engine runs on the deterministic hash fallback (`semantic:
    /// false`) until then.
    EmbedderModel,
    /// Scaffold the project (`.coding/` + the `agent.md` template) —
    /// optional: engines work on plain directories (G3).
    Project,
    /// Install the managed Laya classifier runtime (job 4 D5,
    /// §5.8 :554): uv → venv → `laya[serve]` → the English checkpoint,
    /// then the loopback sidecar. Never in the missing list — the
    /// engine runs without a classifier; this step only tags
    /// [`SetupEvent`]s from [`Setup::classifier_setup`].
    LayaRuntime,
}

impl fmt::Display for SetupStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetupStep::DefaultModel => write!(
                f,
                "a default model (an endpoint with a key if its kind needs one, \
                 plus [general] default_model)"
            ),
            SetupStep::EmbedderModel => {
                write!(f, "the configured embedding model (install it)")
            }
            SetupStep::Project => write!(f, "a scaffolded project (.coding + agent.md)"),
            SetupStep::LayaRuntime => {
                write!(f, "the managed Laya classifier runtime")
            }
        }
    }
}

/// A setup-pipeline progress event (§5.8 :588-589) — binding-ready: the
/// installers (the classifier D5, the embedder D6) stream `Progress`
/// while they run, then one terminal `Done`/`Failed` naming the step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SetupEvent {
    /// A pipeline is making progress. `fraction` is a best-effort
    /// 0.0–1.0 (`None` = indeterminate).
    Progress {
        /// The pipeline's step.
        step: SetupStep,
        /// A human-facing label (the asset being fetched, the phase).
        label: String,
        /// Best-effort completion fraction.
        fraction: Option<f64>,
    },
    /// The step completed.
    Done {
        /// The completed step.
        step: SetupStep,
    },
    /// The step failed.
    Failed {
        /// The failed step.
        step: SetupStep,
        /// The failure's description.
        error: String,
    },
}

/// One endpoint's readiness (§5.8 :564).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EndpointReadiness {
    /// The endpoint's config name.
    pub name: String,
    /// Whether a key is saved for it in `keys.toml`.
    pub has_key: bool,
    /// Whether a probe has ever reached it — `None` = not probed
    /// ([`Setup::status`] makes NO network calls; `test_endpoint` (D2)
    /// fills this).
    pub reachable: Option<bool>,
}

/// One endpoint's config wire (§5.8 :533-534, job 4 D2) — the standalone
/// surface's lean shape. The app's GUI DTO carries extra card fields
/// (per-model configs, pricing) and validates through the same lib
/// `validate_endpoint`; `has_api_key` is OUTPUT ONLY
/// ([`Setup::endpoints`] fills it; the upsert path ignores it).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EndpointDto {
    /// The endpoint's config name (unique, non-empty).
    pub name: String,
    /// The kind wire form: `"openai"`, `"local"`, or `"anthropic"`.
    pub kind: String,
    /// The base URL (non-empty; a missing trailing `/` is appended).
    pub base_url: String,
    /// The served model ids (empty entries dropped).
    pub models: Vec<String>,
    /// The endpoint-level context-window cap, if known.
    pub max_context: Option<usize>,
    /// The endpoint-level output-token cap, if known.
    pub max_output_tokens: Option<usize>,
    /// Whether the endpoint serves a multimodal default model.
    pub multimodal: bool,
    /// Whether the endpoint accepts reasoning-effort overrides.
    pub supports_reasoning_effort: bool,
    /// The endpoint-level default reasoning effort.
    pub reasoning_effort: Option<String>,
    /// The Anthropic workspace id (cleared for other kinds).
    pub workspace_id: Option<String>,
    /// OUTPUT ONLY: whether a key is available for this endpoint.
    pub has_api_key: bool,
}

impl EndpointDto {
    /// Validate + convert into the lib [`crate::config::Endpoint`] —
    /// the same `validate_endpoint` the GUI's save path uses (one
    /// implementation). The lean surface carries no per-model
    /// configs: model ids become bare specs.
    fn into_endpoint(self) -> Result<crate::config::Endpoint, String> {
        crate::config::patch::validate_endpoint(
            &self.name,
            &self.kind,
            &self.base_url,
            self.models,
            Vec::new(),
            self.max_context,
            self.max_output_tokens,
            self.multimodal,
            self.supports_reasoning_effort,
            self.reasoning_effort,
            self.workspace_id,
        )
    }
}

/// The engine-relevant config view (G9, §5.9.9 :733-738, job 4 D3) —
/// what [`Setup::settings`] exposes: every section EXCEPT the
/// app-owned `[ui]` table (row 33: `[ui]` is read only by src-tauri; a
/// standalone consumer has no GUI). On disk nothing moves — the same
/// `config.toml` tables; this view drops the `[ui]` half.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// The `[general]` section — `GeneralSection` holds no `[ui]`
    /// fields (the `[ui]` table lives beside it in `GeneralConfig`).
    pub general: crate::config::general::GeneralSection,
    /// The `[models]` section (the routing/executing/free lanes).
    pub models: crate::config::general::ModelsConfig,
    /// The `[context]` section.
    pub context: crate::config::general::ContextConfig,
    /// The `[trace]` section.
    pub trace: crate::config::general::TraceConfig,
    /// The memory search config (`[memory]`).
    pub memory: crate::memory::MemorySearchConfig,
    /// The `[shell_filter]` section.
    pub shell_filter: crate::config::general::ShellFilterConfig,
    /// The endpoints (`endpoints.toml`) — the lean [`EndpointDto`]
    /// wire with key availability.
    pub endpoints: Vec<EndpointDto>,
}

/// The live-engine pieces a [`Setup::patch_settings`] applies to (D4,
/// §5.8 :538-541) — Arc-cloned out of the engine's brain by
/// [`Setup::for_engine`], so the Setup drives the live-apply without
/// owning the engine (every Arc is shared with the live runtime).
#[derive(Clone)]
struct LiveHandles {
    /// The shared agent factory (the vision slot + the live toggles +
    /// the core-operations/shell-filter/browser-inspection handles).
    factory: Arc<crate::agent::factory::AgentLoopFactory>,
    /// The concrete memory store (the embedder swap).
    memory_store: Option<Arc<crate::memory::MemoryStore>>,
    /// The shared embedder status (Ready gates the re-embed).
    embedder_status: Arc<std::sync::RwLock<crate::memory::embedder::EmbedderStatus>>,
    /// The per-context model resolver.
    model_resolver: Arc<crate::model_resolver::ConfigModelResolver>,
    /// The live safety mode (the approval gates read it per request).
    safety_mode: Arc<std::sync::RwLock<SafetyMode>>,
    /// The shared LLM request trace (the memory limits).
    trace: Arc<crate::provider::trace::LlmRequestLog>,
    /// The brain's shared config — every later spawn reads it.
    config: Arc<tokio::sync::Mutex<Config>>,
    /// The brain's classifier slot (D5) — the classifier-setup
    /// autostart arm swaps it to the fresh sidecar endpoint, the same
    /// swap the GUI's `laya_setup` performs.
    classifier_slot: Option<Arc<std::sync::RwLock<Option<Arc<dyn Classifier>>>>>,
}

impl fmt::Debug for LiveHandles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LiveHandles { factory, memory_store, embedder_status, … }")
    }
}

/// The semantic-search leg (§5.8 :567): which bundled model is
/// configured, whether it is installed in the cache, the embedder's
/// status, and whether semantic (vs. hash-fallback) vectors are live.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedderReadiness {
    /// The configured bundled model id (`None` = the hash fallback —
    /// the `"hash"` sentinel is filtered exactly like the engine does).
    pub configured: Option<String>,
    /// Whether the configured model is present in the cache (always
    /// `false` in a build without the `embeddings` feature — semantic
    /// search is structurally unavailable there).
    pub installed: bool,
    /// The embedder status (§5.8 :567).
    pub status: EmbedderStatus,
    /// Whether real semantic vectors are live (`false` = the
    /// deterministic hash fallback — keyword ranking).
    pub semantic: bool,
}

/// The project leg (§5.8 :569) — plain fs facts about a workspace root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectReadiness {
    /// A `.git` exists at the root.
    pub is_git_repo: bool,
    /// A `.coding/` dir exists.
    pub coding_dir: bool,
    /// The scaffold is in place (`.coding/plans/`).
    pub scaffolded: bool,
    /// The root `agent.md` constitution exists.
    pub agent_md: bool,
    /// A codegraph index exists (`.coding/codegraph.db`).
    pub codegraph_indexed: bool,
}

/// The readiness snapshot (§5.8 :562-571) — what a setup UI shows
/// first: where the config lives, which endpoints exist + have keys,
/// which model is default, whether the engine can run NOW, the
/// semantic-search + classifier legs, the project facts, and the
/// ordered to-do list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetupStatus {
    /// The resolved config dir backing this report.
    pub config_dir: PathBuf,
    /// One readiness row per configured endpoint.
    pub endpoints: Vec<EndpointReadiness>,
    /// `[general] default_model` (the model an unqualified spawn uses).
    pub default_model: Option<String>,
    /// Whether the engine can run on this config: the default model is
    /// served by an endpoint that has it, with a key when its kind
    /// needs one (§5.8 :563).
    pub ready_to_run: bool,
    /// The semantic-search leg (a missing model is NOT fatal — the
    /// engine runs on the hash fallback, `semantic: false`).
    pub embedder: EmbedderReadiness,
    /// The classifier leg (D5): the LIVE managed-runtime status — the
    /// brain's shared Arc when the Setup is engine-backed, this
    /// Setup's own pipeline status otherwise. A config-enabled
    /// install without a live runtime reads `Disabled` (honest: no
    /// classifier call is attempted until
    /// [`Setup::classifier_setup`] or an attached engine lights one).
    pub classifier: ClassifierStatus,
    /// The project facts, present when a workspace root was given.
    pub project: Option<ProjectReadiness>,
    /// The ordered first-run to-do list (§5.8 :570).
    pub missing: Vec<SetupStep>,
}

/// The standalone setup + config handle (§5.8 :525-527) — a pure
/// config-dir handle in D1 (no engine state); the mutating surface
/// (D2+) grows on it, and `for_engine` (D4) attaches a live engine for
/// the apply-immediately legs.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The resolved config dir (explicit / `$MNEMO_HOME` / `~/.mnemo`).
    config_dir: PathBuf,
    /// Where keys come from (D2, §5.8 :578-580) — `File` (`keys.toml`)
    /// is the default; `Env`/`InMemory` are host-managed READ sources
    /// (the write paths refuse them).
    key_source: KeySource,
    /// The live-engine handles (D4, §5.8 :527) — `None` until
    /// [`Setup::for_engine`]; `patch_settings` applies through them.
    live: Option<LiveHandles>,
    /// The managed Laya classifier runtime for this config dir (D5,
    /// §5.8 :554) — rooted at `<config_dir>/laya` (install isolation,
    /// G4's discipline).
    laya: Arc<LayaManager>,
    /// The classifier status the manager drives — a fresh `Disabled`
    /// handle; `for_engine` re-roots it to the brain's shared Arc so
    /// the engine and any GUI attached to it see the same moves.
    classifier_status: Arc<std::sync::RwLock<ClassifierStatus>>,
    /// The embedder status the install/select surface drives (D6) —
    /// fresh `Checking` (no live embedder yet); `for_engine` re-roots
    /// it to the brain's shared Arc.
    embedder_status_arc: Arc<std::sync::RwLock<EmbedderStatus>>,
}

impl Setup {
    /// Open a Setup against a config dir (§5.8 :527): `None` resolves to
    /// `$MNEMO_HOME`/`~/.mnemo` — the ONE resolver the engine shares —
    /// and the dir is created when missing (`Config::load` tolerates
    /// absent files: a fresh install is a valid, if not-ready,
    /// install).
    pub fn open(config_dir: Option<PathBuf>) -> Result<Self, EngineError> {
        let config_dir = crate::config::resolve_config_dir(config_dir);
        std::fs::create_dir_all(&config_dir).map_err(|e| {
            EngineError::start_failed(format!(
                "cannot create config dir '{}': {e}",
                config_dir.display()
            ))
        })?;
        let classifier_status = Arc::new(std::sync::RwLock::new(ClassifierStatus::Disabled));
        let embedder_status_arc =
            Arc::new(std::sync::RwLock::new(EmbedderStatus::Checking));
        let laya = Arc::new(LayaManager::with_base(
            config_dir.join("laya"),
            Arc::clone(&classifier_status),
        ));
        Ok(Self {
            config_dir,
            key_source: KeySource::default(),
            live: None,
            laya,
            classifier_status,
            embedder_status_arc,
        })
    }

    /// The resolved config dir this Setup targets.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Point this Setup at a different [`KeySource`] (§5.8 :578-580):
    /// `Env`/`InMemory` serve `has_api_key` reads without plaintext on
    /// disk — they are host-managed, so the write paths
    /// ([`Setup::set_api_key`], [`Setup::upsert_endpoint`]'s key)
    /// refuse; inject the keys at construction instead.
    pub fn with_key_source(mut self, source: KeySource) -> Self {
        self.key_source = source;
        self
    }

    /// One endpoint's lean DTO — shared by [`Setup::endpoints`] and
    /// [`Setup::settings`].
    fn endpoint_dto(e: &crate::config::Endpoint, has_key: bool) -> EndpointDto {
        EndpointDto {
            name: e.name.clone(),
            kind: crate::config::kind_wire(e.kind).to_string(),
            base_url: e.base_url.clone(),
            models: e.models.iter().map(|m| m.id.clone()).collect(),
            max_context: e.max_context,
            max_output_tokens: e.max_output_tokens,
            multimodal: e.multimodal,
            supports_reasoning_effort: e.supports_reasoning_effort,
            reasoning_effort: e.reasoning_effort.clone(),
            workspace_id: e.workspace_id.clone(),
            has_api_key: has_key,
        }
    }

    /// The engine-relevant config (§5.8 :539, G9 §5.9.9) — every
    /// section EXCEPT the app-owned `[ui]` table (row 33: `Setup`
    /// exposes only `EngineConfig`).
    pub fn settings(&self) -> Result<EngineConfig, EngineError> {
        let config = self.load_config()?;
        let keys_path = self.config_dir.join("keys.toml");
        Ok(EngineConfig {
            general: config.general.general.clone(),
            models: config.general.models.clone(),
            context: config.general.context.clone(),
            trace: config.general.trace.clone(),
            memory: config.general.memory.clone(),
            shell_filter: config.general.shell_filter.clone(),
            endpoints: config
                .endpoints
                .iter()
                .map(|e| Self::endpoint_dto(e, self.key_source.has_key(&keys_path, &e.name)))
                .collect(),
        })
    }

    /// Attach a LIVE engine (§5.8 :527, job 4 D4) — the Arc handles its
    /// brain owns, Arc-cloned into this Setup — so
    /// [`Setup::patch_settings`] applies on the very next request
    /// instead of the next start. The Setup holds the shared Arcs,
    /// never the engine itself.
    pub fn for_engine(&self, engine: &crate::headless::Engine) -> Self {
        let brain = engine.brain();
        Self {
            config_dir: self.config_dir.clone(),
            key_source: self.key_source.clone(),
            // Re-root onto the brain's shared status Arc — the engine
            // and any GUI attached to it see the same moves; the manager
            // still roots at `<config_dir>/laya`.
            laya: Arc::new(LayaManager::with_base(
                self.config_dir.join("laya"),
                brain.classifier_status.clone(),
            )),
            classifier_status: brain.classifier_status.clone(),
            embedder_status_arc: brain.embedder_status.clone(),
            live: Some(LiveHandles {
                factory: brain.factory.clone(),
                memory_store: brain.own_memory_store.clone(),
                embedder_status: brain.embedder_status.clone(),
                model_resolver: brain.model_resolver.clone(),
                safety_mode: brain.safety_mode.clone(),
                trace: brain.trace.clone(),
                config: brain.config.clone(),
                classifier_slot: Some(brain.classifier_slot.clone()),
            }),
        }
    }

    /// Validate + persist + LIVE-apply a settings patch (§5.8 :540) —
    /// the same rules the GUI's save path enforces
    /// (`validate_and_apply_settings_patch`), then the exact live-apply
    /// sequence: the safety mode → the moved rewire core (vision +
    /// embedder + the factory's live toggles) → the model resolver →
    /// the factory's core-operations/shell-filter/browser-inspection
    /// handles → the trace limits → the brain's shared config. Without
    /// a live engine ([`Setup::open`], a first-run flow) only the
    /// persist happens. Effective on the next request.
    ///
    /// The managed-CLASSIFIER flag does not ride this patch (review
    /// round 1, finding 3): a patch carrying `laya_enabled` is
    /// REJECTED whole with code `unsupported_patch` — the shared
    /// validator would silently HALF-apply it (persist + config swap,
    /// but no sidecar stop/start, no slot swap, no status). The whole
    /// leg — flag, runtime, slot, status — lives on
    /// [`Setup::classifier_setup`], the headless twin of the GUI's
    /// Download button (in the GUI the flag rides `save_settings`,
    /// which owns the full rewire leg; the GUI's two surfaces meet in
    /// one process, the headless two in one method).
    pub async fn patch_settings(&self, patch: SettingsSaveDto) -> Result<(), EngineError> {
        if patch.laya_enabled.is_some() {
            return Err(EngineError::unsupported_patch(
                "laya_enabled does not ride the settings patch — use \
                 Setup::classifier_setup(enable), which owns the flag AND the sidecar \
                 stop/start + the live slot + the status",
            ));
        }
        let current = self.load_config()?;
        let new_config =
            crate::config::settings_dto::validate_and_apply_settings_patch(&current, &patch)
                .map_err(EngineError::start_failed)?;
        new_config
            .save_all(&self.config_dir)
            .map_err(|e| EngineError::start_failed(e.to_string()))?;
        let Some(live) = &self.live else {
            return Ok(());
        };
        // Safety mode (the engine's approval gates read the flag per
        // request; the app additionally auto-resolves pending IPC
        // approvals — a headless engine has none parked by the GUI).
        *live.safety_mode.write().expect("safety mode lock poisoned") =
            new_config.general.general.safety;
        // The rewire core: vision + embedder + the live toggles. The
        // re-embed future spawns on tokio (the headless engine runs on
        // tokio by construction).
        let targets = crate::host::rewire::RewireTargets {
            factory: Some(live.factory.clone()),
            memory_store: live.memory_store.clone(),
            embedder_status: live.embedder_status.clone(),
            model_resolver: Some(live.model_resolver.clone()),
            // D6: the embedder cache derives from THIS Setup's config
            // dir — install isolation for custom-dir Setups.
            config_dir: Some(self.config_dir.clone()),
        };
        if let Some(task) = crate::host::rewire::rewire_vision_embedder(&targets, &new_config) {
            tokio::spawn(task);
        }
        // `[models]` overrides on the next request.
        crate::host::rewire::sync_model_resolver(Some(&live.model_resolver), &new_config);
        // The factory's shared tool handles.
        live.factory
            .set_core_operations(new_config.general.git.core_operations.clone());
        live.factory
            .set_shell_filter_config(new_config.general.shell_filter.clone());
        live.factory
            .set_browser_inspection(new_config.general.general.enable_browser_inspection);
        // The trace limits (retroactive budget; the cap from now on) —
        // only when the patch carries trace fields (the GUI's contract).
        if patch.trace_memory_budget_mb.is_some() || patch.trace_request_body_cap_kb.is_some() {
            live.trace
                .set_memory_budget(new_config.general.trace.memory_budget_mb * 1024 * 1024);
            live.trace
                .set_request_body_cap(new_config.general.trace.request_body_cap_kb * 1024);
        }
        // The brain's shared config: every later spawn reads the
        // reloaded values.
        *live.config.lock().await = new_config;
        Ok(())
    }

    /// The managed-Laya runtime handle for this Setup's config dir
    /// (rooted at `<config_dir>/laya` — install isolation, G4): stop
    /// the sidecar, read the catalog + installed flags, share the
    /// status Arc. A `for_engine` Setup drives the brain's status.
    pub fn laya_manager(&self) -> &Arc<LayaManager> {
        &self.laya
    }

    /// Enable or disable the managed Laya classifier runtime (§5.8
    /// :554) — the headless twin of Settings → Classifier:
    ///
    /// * `enable = true`: persist `[general.laya] enabled = true` AND
    ///   apply the flip to the LIVE config (review round 1, finding 2:
    ///   the completing pipeline re-reads the live Arc for the
    ///   autostart decision, so the flip must be IN it, not just on
    ///   disk), then claim the setup guard and fire the download +
    ///   install pipeline (uv → venv → `laya[serve]` → the English
    ///   checkpoint). An engine-backed run autostarts the sidecar when
    ///   the LIVE config still selects it at completion (a mid-setup
    ///   disable wins — it flips the same Arc); a standalone Setup
    ///   installs ONLY (nothing consumes a sidecar until an engine
    ///   starts; the completion parks `Disabled`, and the disk keeps
    ///   the flag for the next engine start). The returned receiver
    ///   streams every status change as a [`SetupEvent`] tagged
    ///   [`SetupStep::LayaRuntime`] — Installing / download fractions /
    ///   Starting / Ready / Failed.
    /// * `enable = false`: persist + live-apply `enabled = false`, stop
    ///   the sidecar (a no-op when none runs), CLEAR the live
    ///   classifier slot (the GUI leg's Disabled ⇒ no client — a stale
    ///   handle would send calls to a dead loopback port), park the
    ///   shared status on `Disabled`, and return a receiver carrying
    ///   the single terminal `Done`.
    ///
    /// A second enable while one runs fails fast with
    /// `"setup_in_flight"` (the same guard the GUI's Download button
    /// uses) — the flip still persists + live-applies first
    /// (idempotent). The enable pipeline is network-bound — never
    /// exercised in tests; the disable leg, the guard, and the
    /// live-apply are.
    pub async fn classifier_setup(
        &self,
        enable: bool,
    ) -> Result<EventReceiver<SetupEvent>, EngineError> {
        let (tx, rx) = tokio::sync::broadcast::channel::<SetupEvent>(64);
        // Persist the flip first — idempotent, so a concurrent second
        // enable is harmless before the guard rejects it.
        let mut config = self.load_config()?;
        config.general.general.laya.enabled = enable;
        config
            .save_all(&self.config_dir)
            .map_err(|e| EngineError::start_failed(format!("saving the config failed: {e}")))?;
        // The LIVE apply (review round 1, finding 2): the completing
        // pipeline + every later spawn re-reads the brain's shared Arc —
        // a flip that only reaches disk leaves an engine-backed enable
        // dark and lets a mid-setup disable lose its race. Applied
        // BEFORE the guard so the contract holds even when the guard
        // rejects (the fixture-observable red/green).
        let live = match &self.live {
            Some(live) => {
                *live.config.lock().await = config.clone();
                Some(live)
            }
            None => None,
        };
        let step = SetupStep::LayaRuntime;
        if !enable {
            // Disable: stop the sidecar (a no-op when none runs), clear
            // the live classifier slot (the GUI leg's Disabled ⇒ no
            // client — a stale handle would send calls to a dead
            // loopback port), park the status, report completion.
            self.laya.stop();
            if let Some(live) = live {
                if let Some(slot) = &live.classifier_slot {
                    *slot.write().expect("classifier slot lock poisoned") = None;
                }
            }
            *self
                .classifier_status
                .write()
                .expect("classifier status lock poisoned") = ClassifierStatus::Disabled;
            let _ = tx.send(SetupEvent::Done { step });
            return Ok(EventReceiver { rx });
        }
        // The guard: claimed only AFTER the persist + live apply
        // succeeded, so a failed save never wedges it; the spawned
        // pipeline's `end_setup` releases it.
        if !self.laya.begin_setup() {
            return Err(EngineError::setup_in_flight(
                "a Laya setup is already running",
            ));
        }
        // The autostart decision re-reads the LIVE config at completion
        // (`mnemo::host::laya::spawn_setup_task`). Engine-backed: the
        // brain's shared Arc (the flip is IN it now). Standalone:
        // install-ONLY — nothing consumes a sidecar until an engine
        // starts, so the spawn's view reads disabled (the completion
        // parks `Disabled`; the disk keeps the enabled flag for the
        // next engine start).
        let config_arc = match live {
            Some(live) => Arc::clone(&live.config),
            None => {
                config.general.general.laya.enabled = false;
                Arc::new(tokio::sync::Mutex::new(config))
            }
        };
        let slot = live.and_then(|live| live.classifier_slot.clone());
        let sink: StatusSink = Arc::new(move |s: &ClassifierStatus| {
            let _ = tx.send(Self::classifier_status_to_event(s, SetupStep::LayaRuntime));
        });
        crate::host::laya::spawn_setup_task(
            Some(sink),
            Arc::clone(&self.laya),
            crate::host::laya::english_checkpoint(),
            config_arc,
            slot,
        );
        Ok(EventReceiver { rx })
    }

    /// The LIVE view of the managed-classifier flag — the value the
    /// runtime actually reads (the brain's shared config Arc), which
    /// can differ from disk between a flip and the next engine save.
    /// `None` when no engine is attached (a standalone Setup has no
    /// live view — read [`Setup::settings`] for the disk state).
    pub async fn live_laya_enabled(&self) -> Option<bool> {
        match &self.live {
            Some(live) => Some(live.config.lock().await.general.general.laya.enabled),
            None => None,
        }
    }

    /// Map the core's classifier status onto the setup pipeline's
    /// event (§5.8 :554 + :588-589): install/download phases are
    /// `Progress`, `Ready`/`Disabled` are terminal `Done`, `Failed`
    /// carries a description.
    fn classifier_status_to_event(s: &ClassifierStatus, step: SetupStep) -> SetupEvent {
        match s {
            ClassifierStatus::Disabled | ClassifierStatus::Ready => SetupEvent::Done { step },
            ClassifierStatus::Failed => SetupEvent::Failed {
                step,
                error: "the managed Laya setup failed (see the log under the \
                        config dir's laya/ tree)"
                    .to_string(),
            },
            ClassifierStatus::Installing => SetupEvent::Progress {
                step,
                label: "installing the Laya runtime".to_string(),
                fraction: None,
            },
            ClassifierStatus::Downloading { label, progress } => SetupEvent::Progress {
                step,
                label: format!("downloading {label}"),
                fraction: Some(*progress),
            },
            ClassifierStatus::Starting => SetupEvent::Progress {
                step,
                label: "starting the managed sidecar".to_string(),
                fraction: None,
            },
            ClassifierStatus::FineTuning { label } => SetupEvent::Progress {
                step,
                label: format!("fine-tuning ({label})"),
                fraction: None,
            },
        }
    }

    /// The curated embedding-model catalog, each entry flagged
    /// `installed` against THIS Setup's cache (`<config_dir>/models`,
    /// D6 — the global coupling died). Without the `embeddings`
    /// feature the catalog is empty (the deterministic hash embedder
    /// is the only one).
    pub fn embedding_models(&self) -> Vec<EmbeddingModelDto> {
        crate::host::embed_download::embedding_model_catalog(
            &crate::config::embedder_cache_dir(&self.config_dir),
        )
    }

    /// Download a bundled embedding model in the background (§5.8
    /// :543): progress streams as [`SetupEvent`]s tagged
    /// [`SetupStep::EmbedderModel`] — download fractions, then
    /// `Done`/`Failed`. The model lands in THIS Setup's cache
    /// (`<config_dir>/models`); the live embedder is swapped by the
    /// next settings save (the same split the GUI's Download button
    /// has — the rewire rebuilds from the on-disk model).
    #[cfg(feature = "embeddings")]
    pub fn install_embedding_model(
        &self,
        id: &str,
    ) -> Result<EventReceiver<SetupEvent>, EngineError> {
        let cache_dir = crate::config::embedder_cache_dir(&self.config_dir);
        let info = crate::host::embed_download::embedding_model_catalog(&cache_dir)
            .into_iter()
            .find(|m| m.id == id)
            .ok_or_else(|| {
                EngineError::start_failed(format!("unknown embedding model '{id}'"))
            })?;
        let (tx, rx) = tokio::sync::broadcast::channel::<SetupEvent>(64);
        let sink: EmbedderStatusSink = Arc::new(move |s: &EmbedderStatus| {
            let _ = tx.send(Self::embedder_status_to_event(s, SetupStep::EmbedderModel));
        });
        crate::host::embed_download::spawn_download(
            Some(sink),
            Arc::clone(&self.embedder_status_arc),
            id.to_string(),
            info.size_mb,
            cache_dir,
            crate::host::embed_download::DownloadOutcome::StatusOnly,
        );
        Ok(EventReceiver { rx })
    }

    /// Without the ONNX runtime there is nothing to download — the
    /// deterministic hash embedder is the only one this build runs.
    #[cfg(not(feature = "embeddings"))]
    pub fn install_embedding_model(
        &self,
        id: &str,
    ) -> Result<EventReceiver<SetupEvent>, EngineError> {
        Err(EngineError::start_failed(format!(
            "the embeddings feature is required to download embedding models \
             (this build runs the deterministic hash embedder; '{id}' cannot install)"
        )))
    }

    /// Air-gapped install (§5.8 :544): copy an already-downloaded
    /// model tree from `dir` (the HuggingFace cache layout) into THIS
    /// Setup's cache. Plain fs — no network; works in every build.
    pub fn install_embedding_model_from(
        &self,
        id: &str,
        dir: &Path,
    ) -> Result<(), EngineError> {
        let cache_dir = crate::config::embedder_cache_dir(&self.config_dir);
        crate::host::embed_download::copy_model_from(dir, id, &cache_dir)
            .map(|_| ())
            .map_err(|e| EngineError::start_failed(format!("copying '{id}' failed: {e}")))
    }

    /// Select the embedding model (§5.8 :545): persist the choice +
    /// live-apply — the FULL settings pipeline (validate → persist →
    /// the rewire's embedder rebuild + re-embed + the resolver), the
    /// same one the GUI's save runs. `"hash"` is the explicit
    /// opt-out (keyword-only memory). An unknown id rejects before
    /// touching the config.
    pub async fn select_embedding_model(&self, id: &str) -> Result<(), EngineError> {
        let known = id.eq_ignore_ascii_case(crate::config::EMBEDDING_MODEL_SENTINEL_HASH)
            || self.embedding_models().iter().any(|m| m.id == id);
        if !known {
            return Err(EngineError::start_failed(format!(
                "unknown embedding model '{id}'"
            )));
        }
        let mut patch = SettingsSaveDto::default();
        patch.bundled_embedding_model = Some(id.to_string());
        self.patch_settings(patch).await
    }

    /// The LIVE embedder status (§5.8 :546) — the brain's shared Arc
    /// when engine-backed, this Setup's own install/select pipeline
    /// otherwise.
    pub fn embedder_status(&self) -> EmbedderStatus {
        self.embedder_status_arc
            .read()
            .expect("embedder status lock poisoned")
            .clone()
    }

    /// Re-embed all memories with the CURRENT config's embedder
    /// (§5.8 :547) — the live-store swap the settings save performs,
    /// re-run without a config change (e.g. after an air-gapped install
    /// made the configured model loadable). Streams the status as
    /// [`SetupEvent`]s tagged [`SetupStep::EmbedderModel`]. Needs the
    /// live store — a standalone Setup rejects.
    #[cfg(feature = "embeddings")]
    pub async fn reembed(&self) -> Result<EventReceiver<SetupEvent>, EngineError> {
        let Some(live) = &self.live else {
            return Err(EngineError::start_failed(
                "no live engine attached — reembed needs the live memory store \
                 (call for_engine first)",
            ));
        };
        let Some(store) = live.memory_store.clone() else {
            return Err(EngineError::start_failed(
                "the attached engine runs without the memory store — reembed has \
                 nothing to swap",
            ));
        };
        let config = self.load_config()?;
        let status = Arc::clone(&self.embedder_status_arc);
        let cache_dir = crate::config::embedder_cache_dir(&self.config_dir);
        let (tx, rx) = tokio::sync::broadcast::channel::<SetupEvent>(64);
        let sink: EmbedderStatusSink = Arc::new(move |s: &EmbedderStatus| {
            let _ = tx.send(Self::embedder_status_to_event(s, SetupStep::EmbedderModel));
        });
        let task_sink = Arc::clone(&sink);
        tokio::spawn(async move {
            // The build flips the shared status itself (Ready or the
            // hash fallback); the install does the fingerprint check
            // + re-embed + swap — the settings save's sequence minus
            // the config change.
            let built = tokio::task::spawn_blocking(move || {
                crate::provider::client_factory::build_embedder(&config, &cache_dir, status)
            })
            .await;
            match built {
                Ok(embedder) => {
                    crate::host::embed_download::install_embedder(store, embedder).await;
                }
                Err(e) => {
                    eprintln!("error: the reembed build task panicked: {e}");
                    task_sink(&EmbedderStatus::Failed);
                }
            }
        });
        Ok(EventReceiver { rx })
    }

    /// Without the ONNX runtime there is no re-embed to run — the hash
    /// embedder is the only one, and its vectors are already stored.
    #[cfg(not(feature = "embeddings"))]
    pub async fn reembed(&self) -> Result<EventReceiver<SetupEvent>, EngineError> {
        Err(EngineError::start_failed(
            "the embeddings feature is required to re-embed (this build runs the \
             deterministic hash embedder only)",
        ))
    }

    /// Set or clear the vision model (§5.8 :549): `Some` sets/replaces
    /// (`{ endpoint, model }`), `None` clears. Rides the full settings
    /// pipeline (validate → persist → the rewire's vision rebuild).
    pub async fn vision_model(&self, m: Option<VisionModelDto>) -> Result<(), EngineError> {
        let mut patch = SettingsSaveDto::default();
        match m {
            Some(vm) => patch.vision_model = Some(vm),
            None => patch.clear_vision_model = true,
        }
        self.patch_settings(patch).await
    }

    /// Map the embedder status onto the setup pipeline's event (§5.8
    /// :543/:547): the download fractions are `Progress`, `Ready` is
    /// terminal `Done`, `Failed` carries a description. The interim
    /// states (`Checking`/`Pulling`) surface as progress; the
    /// `Fallback` degradation as a failed step.
    #[cfg(feature = "embeddings")]
    fn embedder_status_to_event(s: &EmbedderStatus, step: SetupStep) -> SetupEvent {
        match s {
            EmbedderStatus::Ready => SetupEvent::Done { step },
            EmbedderStatus::Failed => SetupEvent::Failed {
                step,
                error: "the embedding model failed to load (see the config dir's \
                        models/ tree)"
                    .to_string(),
            },
            EmbedderStatus::Checking => SetupEvent::Progress {
                step,
                label: "checking the embedding model".to_string(),
                fraction: None,
            },
            EmbedderStatus::Pulling => SetupEvent::Progress {
                step,
                label: "pulling the embedding model".to_string(),
                fraction: None,
            },
            EmbedderStatus::Downloading { model, progress } => SetupEvent::Progress {
                step,
                label: format!("downloading {model}"),
                fraction: Some(*progress),
            },
            EmbedderStatus::Fallback => SetupEvent::Failed {
                step,
                error: "the embedder degraded to the hash fallback".to_string(),
            },
        }
    }

    /// §5.8 :556 — initialize a project directory at `root`: scaffold
    /// `.coding/` (plans + the shipped skills) + the kernel scaffold
    /// core's git-file entries + the root `agent.md` template, and
    /// (when `opts.register`) add the project to `projects.toml`.
    /// Idempotent: every piece is write-if-missing / merge-append, so a
    /// re-run only self-heals deleted artifacts.
    pub fn init_project(
        &self,
        root: &Path,
        opts: InitProjectOptions,
    ) -> Result<ProjectInfo, EngineError> {
        let fs = |e: std::io::Error| {
            EngineError::start_failed(format!("initializing the project failed: {e}"))
        };
        let coding_dir = root.join(".coding");
        std::fs::create_dir_all(&coding_dir).map_err(fs)?;
        std::fs::create_dir_all(coding_dir.join("plans")).map_err(fs)?;
        crate::project::skills_seed::seed_skills(&coding_dir.join("skills"))
            .map_err(|e| {
                EngineError::start_failed(format!("seeding the skills failed: {e}"))
            })?;
        crate::project::scaffold::seed_git_files(root).map_err(|e| {
            EngineError::start_failed(format!("seeding the git files failed: {e}"))
        })?;
        let agent_md = root.join("agent.md");
        if !agent_md.exists() {
            std::fs::write(&agent_md, crate::project::agent_md::DEFAULT_PROJECT_TEMPLATE)
                .map_err(fs)?;
        }
        let registered = if opts.register {
            let registry_path = self.config_dir.join("projects.toml");
            let mut registry =
                crate::config::ProjectRegistry::load_or_default(&registry_path).map_err(
                    |e| EngineError::start_failed(format!("loading projects.toml failed: {e}")),
                )?;
            let name = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string());
            registry.add(&name, &root.display().to_string());
            registry.save(&registry_path).map_err(|e| {
                EngineError::start_failed(format!("saving projects.toml failed: {e}"))
            })?;
            true
        } else {
            false
        };
        Ok(ProjectInfo {
            root: root.to_path_buf(),
            coding_dir,
            agent_md,
            registered,
        })
    }

    /// §5.8 :557 — read an agent.md constitution. `Scope::Global` maps to
    /// `<config_dir>/agent.md` (install isolation), `Scope::Project` to
    /// the project root's. A missing file reads as the empty string.
    pub fn agent_md(&self, scope: Scope) -> String {
        let path = match scope {
            Scope::Global => self.config_dir.join("agent.md"),
            Scope::Project(root) => root.join("agent.md"),
        };
        std::fs::read_to_string(&path).unwrap_or_default()
    }

    /// §5.8 :558 — write an agent.md constitution (creating the file;
    /// the parent must exist for a project scope — `init_project`
    /// creates it).
    pub async fn set_agent_md(&self, scope: Scope, text: String) -> Result<(), EngineError> {
        let path = match scope {
            Scope::Global => self.config_dir.join("agent.md"),
            Scope::Project(root) => root.join("agent.md"),
        };
        std::fs::write(&path, text).map_err(|e| {
            EngineError::start_failed(format!("writing {} failed: {e}", path.display()))
        })
    }

    /// §5.8 :559 — read a project's safety rules
    /// (`<project>/.coding/safety.toml`, the `ApproveViaApi` set). A
    /// missing file reads as the empty DTO; a malformed one degrades to
    /// empty (same best-effort letter as the agent's own loader — the
    /// live rules skip invalid entries rather than fail the session).
    pub async fn safety_rules(&self, project: &Path) -> SafetyRulesDto {
        let path = project.join(".coding").join("safety.toml");
        SafetyRulesDto {
            rules: crate::safety_rules::read_rules(&path)
                .unwrap_or_default()
                .into_iter()
                .map(|r| SafetyRuleDto {
                    tool: r.tool,
                    pattern: r.pattern,
                    kind: r.kind,
                })
                .collect(),
        }
    }

    /// §5.8 :560 — REPLACE a project's safety rules (writes
    /// `<project>/.coding/safety.toml` in the kernel's exact format; a
    /// live `SafetyRules` re-reads it on the next dispatch via the
    /// mtime check).
    pub async fn set_safety_rules(
        &self,
        project: &Path,
        rules: SafetyRulesDto,
    ) -> Result<(), EngineError> {
        let path = project.join(".coding").join("safety.toml");
        let entries: Vec<crate::safety_rules::RuleEntry> = rules
            .rules
            .into_iter()
            .map(|r| crate::safety_rules::RuleEntry {
                tool: r.tool,
                pattern: r.pattern,
                kind: r.kind,
            })
            .collect();
        crate::safety_rules::write_rules(&path, &entries).map_err(|e| {
            EngineError::start_failed(format!("writing the safety rules failed: {e}"))
        })
    }

    /// §5.8 :551 — list the configured MCP servers (the global file +
    /// the project's, when given), each tagged with its source, the
    /// project definition winning by name. A listing never fails —
    /// read errors degrade to the readable set.
    pub fn mcp_servers(&self, project: Option<&Path>) -> Vec<McpServerWire> {
        let global = self.config_dir.join("mcp.toml");
        let project_path = project.map(|p| p.join(".coding").join("mcp.toml"));
        crate::config::mcp::list_wires(&global, project_path.as_deref())
    }

    /// §5.8 :551 — save the MCP servers split by source (the exact core
    /// the GUI's save runs, moved lib-side): global entries to
    /// `<config_dir>/mcp.toml`, project entries to the project's
    /// `.coding/mcp.toml` (never materialized for a purely-global save),
    /// shadowed global twins restored. Live-apply when an engine is
    /// attached: the shared config's `mcp` field + the factory's manager
    /// swap (agents built after this save see the new set).
    pub async fn save_mcp_servers(
        &self,
        project: Option<&Path>,
        servers: Vec<McpServerWire>,
    ) -> Result<String, EngineError> {
        let global_path = self.config_dir.join("mcp.toml");
        let project_path = project.map(|p| p.join(".coding").join("mcp.toml"));
        let outcome =
            crate::config::mcp::save_split(&global_path, project_path.as_deref(), servers)
                .map_err(|e| {
                    EngineError::start_failed(format!("saving the MCP servers failed: {e}"))
                })?;
        if let Some(live) = &self.live {
            let mut config = live.config.lock().await;
            config.mcp = outcome.global;
            if let Some(manager) = live.factory.mcp_manager() {
                manager.replace_servers(outcome.merged.clone()).await;
            }
        }
        Ok(
            "saved — servers apply to agents built after this save (a restart always picks them \
             up); use Test to verify a connection now"
                .into(),
        )
    }

    /// §5.8 :552 — connect to the named MCP server and list its tools
    /// (the 45 s budget the GUI's Test uses). With an engine attached the
    /// factory's LIVE manager answers; a standalone Setup builds a
    /// transient manager over the current global file.
    pub async fn test_mcp(&self, name: &str) -> Result<McpTestResult, EngineError> {
        let name = name.trim().to_string();
        let manager = match self.live.as_ref().and_then(|live| live.factory.mcp_manager()) {
            Some(manager) => manager,
            None => {
                let defs = crate::config::mcp::load_or_default(
                    &self.config_dir.join("mcp.toml"),
                )
                .map_err(|e| {
                    EngineError::start_failed(format!("reading the MCP config failed: {e}"))
                })?;
                std::sync::Arc::new(crate::mcp::McpManager::from_config(&defs))
            }
        };
        let fail = |msg: String| McpTestResult {
            ok: false,
            tool_count: 0,
            tool_names: Vec::new(),
            error: Some(msg),
        };
        match tokio::time::timeout(std::time::Duration::from_secs(45), manager.server_tools(&name))
            .await
        {
            Ok(Ok(tools)) => Ok(McpTestResult {
                ok: true,
                tool_count: tools.len(),
                tool_names: tools.iter().map(|t| t.name.clone()).collect(),
                error: None,
            }),
            Ok(Err(e)) => Ok(fail(e.to_string())),
            Err(_) => Ok(fail(
                "timed out after 45s connecting / listing tools — is the command on PATH and \
                 does the server start?"
                    .into(),
            )),
        }
    }

    /// Load the config for the endpoints group — one error mapping for
    /// every read.
    fn load_config(&self) -> Result<Config, EngineError> {
        Config::load(&self.config_dir).map_err(|e| {
            EngineError::start_failed(format!(
                "config load from '{}' failed: {e}",
                self.config_dir.display()
            ))
        })
    }

    /// The configured endpoints (§5.8 :533) with per-endpoint key
    /// availability — the lean [`EndpointDto`] wire.
    pub fn endpoints(&self) -> Result<Vec<EndpointDto>, EngineError> {
        let config = self.load_config()?;
        let keys_path = self.config_dir.join("keys.toml");
        Ok(config
            .endpoints
            .iter()
            .map(|e| Self::endpoint_dto(e, self.key_source.has_key(&keys_path, &e.name)))
            .collect())
    }

    /// Upsert one endpoint (§5.8 :534): the same lib validation the
    /// GUI's save path enforces (`validate_endpoint` + the SET
    /// re-validation — the default provider/model must still resolve),
    /// replace-or-append by name, then persist. `key`: `Some` stores
    /// the key alongside (the `File` source only); `None` PRESERVES
    /// the endpoint's existing key.
    pub fn upsert_endpoint(
        &self,
        dto: EndpointDto,
        key: Option<Secret>,
    ) -> Result<(), EngineError> {
        let endpoint = dto.into_endpoint().map_err(EngineError::start_failed)?;
        let current = self.load_config()?;

        // Preserve the set's order: replace-or-append by name.
        let mut built = current.endpoints.clone();
        match built.iter().position(|e| e.name == endpoint.name) {
            Some(i) => built[i] = endpoint.clone(),
            None => built.push(endpoint.clone()),
        }

        // The surviving endpoints' keys merged with the provided one
        // (apply_endpoints rebuilds the store keeping keys for
        // existing endpoints only).
        let mut api_keys: HashMap<String, String> = HashMap::new();
        for ep in &built {
            if let Some(k) = current.keys.get(&ep.name) {
                api_keys.insert(ep.name.clone(), k.to_string());
            }
        }
        if let Some(secret) = &key {
            match &self.key_source {
                KeySource::File => {
                    api_keys.insert(endpoint.name.clone(), secret.expose().to_string());
                }
                _ => {
                    return Err(EngineError::start_failed(
                        "storing a key requires the File key source — Env and InMemory \
                         sources are host-managed (inject keys at construction)",
                    ));
                }
            }
        }

        let default_provider = current.general.general.default_provider.clone();
        let default_model = current.general.general.default_model.clone();
        crate::config::patch::validate_endpoint_set(
            &built,
            default_provider.as_deref(),
            default_model.as_deref(),
            default_model.as_deref(),
        )
        .map_err(EngineError::start_failed)?;
        let new_config = crate::config::patch::apply_endpoints(
            &current,
            built,
            default_provider,
            default_model,
            &api_keys,
        );
        new_config
            .save_all(&self.config_dir)
            .map_err(|e| EngineError::start_failed(e.to_string()))?;
        Ok(())
    }

    /// Remove one endpoint by name (§5.8 :534): its key drops with it
    /// (the rebuilt store keeps keys for surviving endpoints only).
    /// Refused when the default provider still references it (the SET
    /// re-validation).
    pub fn remove_endpoint(&self, name: &str) -> Result<(), EngineError> {
        let current = self.load_config()?;
        let built: Vec<crate::config::Endpoint> = current
            .endpoints
            .iter()
            .filter(|e| e.name != name)
            .cloned()
            .collect();
        if built.len() == current.endpoints.len() {
            return Err(EngineError::start_failed(format!(
                "endpoint '{name}' does not exist"
            )));
        }
        let mut api_keys: HashMap<String, String> = HashMap::new();
        for ep in &built {
            if let Some(k) = current.keys.get(&ep.name) {
                api_keys.insert(ep.name.clone(), k.to_string());
            }
        }
        let default_provider = current.general.general.default_provider.clone();
        let default_model = current.general.general.default_model.clone();
        crate::config::patch::validate_endpoint_set(
            &built,
            default_provider.as_deref(),
            default_model.as_deref(),
            default_model.as_deref(),
        )
        .map_err(EngineError::start_failed)?;
        let new_config = crate::config::patch::apply_endpoints(
            &current,
            built,
            default_provider,
            default_model,
            &api_keys,
        );
        new_config
            .save_all(&self.config_dir)
            .map_err(|e| EngineError::start_failed(e.to_string()))?;
        Ok(())
    }

    /// Store an API key for an endpoint (§5.8 :535) — WRITE-ONLY: there
    /// is no getter (only [`Setup::has_api_key`]). The `File` source
    /// persists to `keys.toml` (atomic write + user-only permissions);
    /// `Env`/`InMemory` refuse (host-managed).
    pub fn set_api_key(&self, endpoint_name: &str, secret: Secret) -> Result<(), EngineError> {
        match &self.key_source {
            KeySource::File => {
                let keys_path = self.config_dir.join("keys.toml");
                let mut store = crate::config::keys::KeyStore::load_or_default(&keys_path)
                    .map_err(|e| EngineError::start_failed(e.to_string()))?;
                store.insert(endpoint_name.to_string(), secret.expose().to_string());
                store
                    .save(&keys_path)
                    .map_err(|e| EngineError::start_failed(e.to_string()))?;
                Ok(())
            }
            _ => Err(EngineError::start_failed(
                "set_api_key requires the File key source — Env and InMemory sources are \
                 host-managed (inject keys at construction)",
            )),
        }
    }

    /// Whether a key is AVAILABLE for an endpoint (§5.8 :535) — never
    /// the value. Consults the [`KeySource`] (file/env/map).
    pub fn has_api_key(&self, endpoint_name: &str) -> bool {
        self.key_source
            .has_key(&self.config_dir.join("keys.toml"), endpoint_name)
    }

    /// Fetch the live model list for an endpoint (§5.8 :536) — the
    /// shared credential resolution (override args first, then the
    /// saved endpoint + `keys.toml` key, then the kind-aware env
    /// fallback, then `"dummy"`), then the kind-selected fetcher.
    /// Never writes config.
    pub async fn list_models(
        &self,
        endpoint_name: &str,
        base_url: Option<String>,
        api_key: Option<String>,
        kind: Option<String>,
    ) -> Result<Vec<crate::provider::models::ModelWithVision>, EngineError> {
        let config = self.load_config()?;
        let (base_url, api_key, kind) =
            crate::provider::models::resolve_endpoint_credentials(
                &config,
                endpoint_name,
                base_url,
                api_key,
                kind,
            )
            .map_err(EngineError::start_failed)?;
        crate::provider::models::fetch_by_kind(&kind, &base_url, &api_key)
            .await
            .map_err(|e| EngineError::start_failed(e.to_string()))
    }

    /// Probe an endpoint's `/models` (§5.8 :536): `true` when the probe
    /// returns a list — `false` on ANY error (the detail rides
    /// [`Setup::list_models`] when a diagnosis is needed).
    pub async fn test_endpoint(
        &self,
        endpoint_name: &str,
        base_url: Option<String>,
        api_key: Option<String>,
        kind: Option<String>,
    ) -> bool {
        self.list_models(endpoint_name, base_url, api_key, kind)
            .await
            .is_ok()
    }

    /// The readiness snapshot (§5.8 :562-571) for this config — and,
    /// when given, a workspace root's project facts. Pure fs + config
    /// reads; NO network calls (endpoints report `reachable: None`
    /// until `test_endpoint` (D2) probes one). The classifier leg is
    /// the LIVE managed-runtime status (D5): a `for_engine` Setup
    /// shares the brain's Arc — Installing/Downloading/Starting/Ready
    /// surface as they happen; a standalone Setup's own pipeline
    /// moves it.
    pub fn status(&self, project: Option<&Path>) -> Result<SetupStatus, EngineError> {
        let config = self.load_config()?;
        let mut status = Self::status_from_config(&self.config_dir, &config, project);
        status.classifier = self
            .classifier_status
            .read()
            .expect("classifier status lock poisoned")
            .clone();
        Ok(status)
    }

    /// The shared status computation — the engine's start precondition
    /// passes its ALREADY-LOADED config (no double load); the public
    /// [`Setup::status`] re-loads from disk.
    pub(crate) fn status_from_config(
        config_dir: &Path,
        config: &Config,
        project: Option<&Path>,
    ) -> SetupStatus {
        let general = &config.general.general;

        let endpoints: Vec<EndpointReadiness> = config
            .endpoints
            .iter()
            .map(|e| EndpointReadiness {
                name: e.name.clone(),
                has_key: config.keys.get(&e.name).is_some(),
                reachable: None,
            })
            .collect();

        // The host rule (patch.rs's `validate_endpoint_set`): the
        // default provider's endpoint, else the first.
        let host = general
            .default_provider
            .as_deref()
            .and_then(|n| config.endpoints.iter().find(|e| e.name == n))
            .or_else(|| config.endpoints.first());

        let default_model = general.default_model.clone().filter(|m| !m.is_empty());

        // ready_to_run (§5.8 :563): the default model served by an
        // endpoint that has it, with a key when its kind needs one
        // (Local endpoints run keyless).
        let ready_to_run = match (&default_model, host) {
            (Some(model), Some(ep)) => {
                ep.has_model(model)
                    && (ep.kind == EndpointKind::Local || config.keys.get(&ep.name).is_some())
            }
            _ => false,
        };

        // The embedder leg (§5.8 :567) — the same sentinel filter the
        // engine applies (engine.rs's configured_embedding_model).
        let configured = general
            .bundled_embedding_model
            .clone()
            .filter(|m| !m.eq_ignore_ascii_case(crate::config::EMBEDDING_MODEL_SENTINEL_HASH));
        let cache_dir = crate::config::embedder_cache_dir(config_dir);
        #[cfg(feature = "embeddings")]
        let installed = configured
            .as_deref()
            .map(|id| crate::memory::embedder::is_model_installed(&cache_dir, id))
            .unwrap_or(false);
        #[cfg(not(feature = "embeddings"))]
        let installed = {
            // No ONNX runtime in this build: semantic search is
            // structurally unavailable — the hash fallback is the only
            // embedder.
            let _ = &cache_dir;
            false
        };
        let semantic = installed;
        let status = if configured.is_none() || installed {
            // No model configured: the deterministic hash embedder is
            // genuinely ready (keyword mode). Installed: the real one.
            EmbedderStatus::Ready
        } else {
            // Configured but missing from the cache — the engine
            // falls back to hash on start; the missing list carries
            // the install step.
            EmbedderStatus::Failed
        };

        // The classifier leg — the STATIC approximation for callers
        // with no live handle (the engine's start gate): enabled means
        // calls are attempted (`ClassifierStatus::Ready`'s contract).
        // The instance path ([`Setup::status`]) OVERRIDES this with
        // the live managed-runtime status (D5).
        let classifier = if general.laya.enabled {
            ClassifierStatus::Ready
        } else {
            ClassifierStatus::Disabled
        };

        let project = project.map(|root| ProjectReadiness {
            is_git_repo: root.join(".git").exists(),
            coding_dir: root.join(".coding").is_dir(),
            scaffolded: root.join(".coding").join("plans").is_dir(),
            agent_md: root.join("agent.md").is_file(),
            codegraph_indexed: root.join(".coding").join("codegraph.db").exists(),
        });

        // The ordered to-do list (§5.8 :570): the fatal leg first.
        let mut missing = Vec::new();
        if !ready_to_run {
            missing.push(SetupStep::DefaultModel);
        }
        if configured.is_some() && !installed {
            missing.push(SetupStep::EmbedderModel);
        }
        if let Some(p) = &project {
            if !p.scaffolded {
                missing.push(SetupStep::Project);
            }
        }

        SetupStatus {
            config_dir: config_dir.to_path_buf(),
            endpoints,
            default_model,
            ready_to_run,
            embedder: EmbedderReadiness {
                configured,
                installed,
                status,
                semantic,
            },
            classifier,
            project,
            missing,
        }
    }

    /// The `not_configured` message's missing list ("a; b; c").
    pub(crate) fn format_missing(missing: &[SetupStep]) -> String {
        missing
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::policy::PermissionPolicy;
    use crate::config::SafetyMode;
    use crate::headless::{Engine, EngineOptions, MemoryBackend};
    use crate::workspace::Workspace;

    /// The ready config the engine fixture needs (the sibling suites'
    /// `write_ready_config` twin — this suite is self-contained). The
    /// endpoint serves a SECOND model so the `default_model` patch
    /// has a visible live change to make.
    fn write_ready_config(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[general]\ndefault_provider = \"test\"\ndefault_model = \"test-model\"\nbundled_embedding_model = \"hash\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("endpoints.toml"),
            "[[endpoint]]\nname = \"test\"\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"test-model\", \"alt-model\"]\n",
        )
        .unwrap();
        std::fs::write(dir.join("keys.toml"), "[test]\napi_key = \"sk-test\"\n").unwrap();
    }

    /// D4's live-apply pin (§5.8 :540): `patch_settings` through a
    /// `for_engine` Setup flips the LIVE brain pieces — the safety flag
    /// + the shared config the next spawn reads — without a restart.
    #[tokio::test]
    async fn patch_settings_live_applies_through_for_engine() {
        let root = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        write_ready_config(cfg.path());
        let engine = Engine::start(EngineOptions {
            workspace: Workspace::new(root.path()),
            config_dir: Some(cfg.path().to_path_buf()),
            permissions: PermissionPolicy::OverridePermissions,
            memory: MemoryBackend::Sqlite { path: None },
            index_codegraph: false,
            require_git: false,
            auto_install_embedder: false,
        })
        .await
        .expect("a ready config starts");
        let setup = Setup::open(Some(cfg.path().to_path_buf()))
            .expect("open")
            .for_engine(&engine);
        let patch = SettingsSaveDto {
            safety: Some("autonomous".into()),
            default_model: Some("alt-model".into()),
            ..Default::default()
        };
        setup.patch_settings(patch).await.expect("patch applies");
        // The LIVE safety flag flipped…
        assert_eq!(
            *engine.brain().safety_mode.read().expect("safety lock"),
            SafetyMode::Autonomous
        );
        // …and the shared config the next spawn reads carries the patch.
        let shared = engine.brain().config.lock().await;
        assert_eq!(
            shared.general.general.default_model.as_deref(),
            Some("alt-model")
        );
        drop(shared);
        engine.shutdown().await.expect("shutdown");
    }
}