// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Per-agent construction — the `AgentLoopFactory`.
//!
//! The factory holds the shared, stateless dependencies (provider, constitution
//! source, memory store, sandbox, safety rules, safety mode, context manager)
//! and builds a **fresh** [`AgentLoop`] on each call to [`build`](Self::build).
//! Each built loop gets its own [`Workflow`] (loading the latest plan from
//! disk) and its own [`ToolRegistry`] (since the workflow tools —
//! `create_plan` / `complete_step` — hold an `Arc<Mutex<Workflow>>`).
//!
//! This is what makes multi-agent safe for concurrent implementation: two
//! agents built from the same factory have **independent** workflow/plan state.
//! Spawning a second agent is `factory.build()` — it shares the provider,
//! memory, sandbox, and safety rules (all concurrency-safe), but gets its own
//! plan.
//!
//! The shared deps are:
//! - `provider` — stateless, safe to share (`Arc<dyn LlmClient>`).
//! - `constitution_source` — re-read from disk per turn via mtime check; the
//!   `ConstitutionSource` is `Clone` and each build gets its own clone (the
//!   cached constitution + mtimes are cheap to duplicate).
//! - `memory` — the SQLite store is behind an internal mutex, safe to share.
//! - `sandbox` — immutable after construction.
//! - `safety_rules` — mtime-checked, read-only from the loop's perspective.
//! - `safety_mode` — shared `Arc<RwLock<SafetyMode>>` so the UI toggle affects
//!   all agents (this is intentional — it's a global runtime setting).
//! - `context_manager` — `Clone` (two `usize`s); each build gets its own copy.
//! - `plans_dir` — the per-agent workflow loads its plan from here.
//! - `spawner` — optional [`AgentSpawner`] (set once at startup by the IPC
//!   layer). When present, every built agent's registry gets a `spawn_agent`
//!   tool so it can start background agents.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tokio::sync::Mutex;

use crate::agent::context::ContextManager;
use crate::agent::{AgentLoop, AgentLoopConfig};
use crate::config::{SafetyMode, ShellFilterConfig};
use crate::memory::knowledge::{self, KnowledgeStore};
use crate::memory::MemoryStoreTrait;
use crate::project::ConstitutionSource;
use crate::provider::vision::{ImageDescriber, SwappableVision};
use crate::provider::{LlmClient, SwappableProvider};
use crate::runtime::AgentSpawner;
use crate::safety_rules::SafetyRules;
use crate::skill::SkillLibrary;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::agent::{
    convert_line_endings::ConvertLineEndingsTool,
    file_edit::FileEditTool,
    file_write::FileWriteTool,
    git::GitTool,
    git_read_tool::GitReadTool,
    image_tools::tools::{
        ImageAnalysisTool, ImageAnalyzeChartTool, ImageDiagnoseErrorTool, ImageExtractTextTool,
        ImageUiDiffTool, ImageUiToArtifactTool, ImageUnderstandDiagramTool,
    },
    list_models::ListModelsTool,
    multi_edit::MultiEditTool,
    read_files::ReadFilesTool,
    search::SearchTool,
    search_read::SearchReadTool,
    shell::ShellTool,
    spawn_agent::SpawnAgentTool,
    web_fetch::WebFetchTool,
    write_review_report::WriteReviewReportTool,
};
use crate::tool::memory::retrieval::MemorySearchTool;
use crate::tool::memory::{
    AutoTypingHandle, MemoryAmendTool, MemoryConsolidateTool, MemoryDeleteTool,
    MemorySupersedeTool, MemoryUpdateTool, MemoryWriteTool,
};
use crate::tool::workflow::ask_user::AskUserTool;
use crate::tool::workflow::plan::{
    AbandonPlanTool, CompleteStepTool, CreatePlanTool, CurrentPlanTool, FinishTool, UpdatePlanTool,
};
use crate::tool::workflow::skill::{
    AbandonSkillTool, SkillCreateTool, SkillEndTool, SkillReloadTool, SkillStartTool,
};
use crate::tool::ToolRegistry;
use crate::workflow::Workflow;

/// The shared backlog wiring handed to every `backlog_add` tool: the store
/// handle (the SAME `Arc` the Tauri `backlog_*` commands lock) plus the
/// optional UI notifier the app layer injects so agent adds emit
/// `backlog://changed` live.
struct BacklogWiring {
    store: Arc<tokio::sync::Mutex<crate::backlog::BacklogStore>>,
    on_changed: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// A per-agent root override for parallel run-all worktree agents
/// (plan ffd7a86f).
///
/// The factory's own `sandbox` / `project_root` / `codegraph` bind every
/// built agent to the MAIN project tree. A worktree agent must instead
/// work in its own git worktree (its own branch), so the run-all spawn
/// path builds it with this spec: the file/shell/search tools, the git
/// tools, and the code graph all bind to the worktree root. Everything
/// else (memory, backlog wiring, skills, provider, spawner, reviews)
/// stays factory-shared — coordination state is in-process and shared by
/// design. The spec is stored on the built [`AgentLoop`] so subagents
/// spawned by that agent inherit the same root (a worktree agent's
/// reviewer must see the worktree's diff, not the main tree's).
#[derive(Clone)]
pub struct AgentRootSpec {
    /// The sandbox (working-tree root) the agent's file/shell/search tools
    /// operate on — the worktree root.
    pub sandbox: Sandbox,
    /// The git root for the `git` / `git_read` tools — the worktree root.
    pub project_root: PathBuf,
    /// The code graph over the worktree tree. `None` mirrors the factory's
    /// optional graph (indexing disabled); the run-all spawn path builds a
    /// fresh graph over the worktree's `.coding/codegraph.db`.
    pub codegraph: Option<Arc<crate::codegraph::CodeGraph>>,
}

/// Builds per-agent [`AgentLoop`]s, each with its own workflow + tool registry.
///
/// The shared, stateless deps live here; `build()` creates a fresh `Workflow`
/// (loading the latest plan from `plans_dir`) + a fresh `ToolRegistry` (wired
/// to that workflow) + a new `AgentLoop` on every call.
pub struct AgentLoopFactory {
    /// The shared provider. Behind an `RwLock` so the model can be swapped at
    /// runtime (the status-bar model picker): the IPC layer builds a new client
    /// and swaps it in, and every subsequent `build()` uses it. Reads are cheap
    /// (one per `build()`).
    provider: RwLock<Arc<dyn LlmClient>>,
    /// The DISPLAY-space effort of the shared default provider slot — stamped
    /// onto every loop built by this factory (the loops' no-override
    /// resolution branch reports it, backlog 51dab4da). Updated alongside
    /// `set_provider` swaps and initialized at startup from the resolved
    /// default model. `None` when unknown (test factories) — the UI falls
    /// back to the endpoint default.
    default_display_effort: RwLock<Option<String>>,
    /// The context manager template. Swapped alongside the provider so the new
    /// model's context window takes effect for freshly built agents.
    context_manager: RwLock<ContextManager>,
    /// The fill rate the context manager was built with — needed to rebuild a
    /// `ContextManager` when the provider (and thus its max context) changes.
    fill_rate: f64,
    /// The proxy cache ceiling (cliff guard) — preserved across model swaps
    /// so the rebuilt `ContextManager` keeps it (same preservation contract
    /// as the preflight settings).
    proxy_cache_ceiling: Option<usize>,
    /// Whether the pre-flight hard-ceiling guard is enabled — preserved across
    /// model swaps so the rebuilt `ContextManager` keeps the setting.
    preflight_compact: bool,
    /// Headroom tokens for the pre-flight guard — preserved across model swaps.
    compact_headroom_tokens: usize,
    constitution_source: ConstitutionSource,
    memory: Option<Arc<dyn MemoryStoreTrait>>,
    /// The knowledge-file backing for typed semantic records — computed from
    /// the plans dir's parent (`.coding/knowledge`, the plans dir's sibling)
    /// once at construction and shared by the memory tools + the finish
    /// capture. `None` when the plans dir implies no project root.
    knowledge: Option<Arc<KnowledgeStore>>,
    sandbox: Arc<Sandbox>,
    project_root: PathBuf,
    safety_rules: Option<Arc<SafetyRules>>,
    safety_mode: Arc<RwLock<SafetyMode>>,
    plans_dir: PathBuf,
    /// Shared swappable vision slot. Always present so Settings can enable,
    /// disable, or re-point the vision model at runtime; agents and the
    /// `describe_image` tool all share this `Arc`.
    vision: Arc<SwappableVision>,
    /// Shared swappable provider slot — a handle to the main LLM client that
    /// sees runtime model swaps. Held (not snapshotted) so tools that need an
    /// LLM at call time (currently `memory_consolidate`) see Settings swaps on
    /// their next call without rebuilding the registry. Mirrors `vision`.
    provider_slot: Arc<SwappableProvider>,
    /// The shared Laya auto-typing gate for `memory_write` (backlog
    /// a147b63c) — `None` until the IPC layer wires it via
    /// `with_auto_typing` (the app runtime's shared classifier slot + the
    /// `auto_type_memories` flag mirror).
    typing: Option<AutoTypingHandle>,
    /// An optional spawner that lets an agent start background agents (the
    /// `spawn_agent` tool). `None` until the IPC layer wires it in via
    /// `set_spawner` — the tool is then omitted from the registry. Behind an
    /// `RwLock` so it can be set once at startup through the shared `Arc`.
    spawner: RwLock<Option<Arc<dyn AgentSpawner>>>,
    /// An optional descendant tracker — the brain-side seam that lets the
    /// dispatch layer gate workflow-state transitions on "no spawned
    /// subagents running". `None` until the IPC layer wires it in via
    /// [`set_descendant_tracker`](Self::set_descendant_tracker). Behind an
    /// `RwLock` so it can be set once at startup through the shared `Arc`.
    descendant_tracker: RwLock<Option<Arc<dyn crate::runtime::DescendantTracker>>>,
    /// An optional shared backlog store handle + UI notifier — the SAME
    /// `Arc<tokio::sync::Mutex<..>>` the Tauri `backlog_*` IPC commands lock,
    /// plus the optional `on_changed` callback the app layer injects so
    /// agent-side `backlog_add` calls emit `backlog://changed` (the library
    /// crate has no Tauri dependency, so the callback is the seam). `None`
    /// until the IPC layer wires it in via
    /// [`set_backlog`](Self::set_backlog); until then the `backlog_add` tool
    /// is omitted from every registry.
    backlog: RwLock<Option<BacklogWiring>>,
    /// The skill library — the `.coding/skills/` dir plus the LIVE registry it
    /// holds. Shared across all agents, and reloadable in place: a skill file
    /// authored mid-session (`skill_create`) or edited by hand + `skill_reload`
    /// is picked up without rebuilding anything. The skill tools consult it to
    /// validate + look up a skill's tool allow-list + prompt. `None` when no
    /// skills dir was configured (tests).
    skills: Option<Arc<SkillLibrary>>,
    /// An optional per-context model resolver. When present, every built agent
    /// gets it wired in so the turn driver can pick a model per workflow state
    /// / skill / subagent. `None` in tests / when no `[models]` section is
    /// configured. Set once at startup via `with_model_resolver`.
    model_resolver: Option<Arc<dyn crate::model_resolver::ModelResolver>>,
    /// The shared headless debug browser. Lazily spawned on first tool call;
    /// the IPC layer injects the *same* `Arc` (via [`browser_slot`]) so the
    /// right-panel Browser tab and the agent drive one browser. Defaults to a
    /// fresh manager when not injected (tests). Only present under the
    /// `browser` feature — the whole chromiumoxide seam compiles out for
    /// coding-only builds.
    #[cfg(feature = "browser")]
    browser: Arc<crate::browser::BrowserManager>,
    /// The per-project code knowledge graph. `None` when indexing is disabled
    /// (`[general] codegraph = false`) or the DB failed to open — the `graph_*`
    /// tools are then omitted from the registry entirely (rather than
    /// erroring at call time). Set once at startup via `with_codegraph`.
    codegraph: Option<Arc<crate::codegraph::CodeGraph>>,
    /// The shared MCP connection manager — lazily connects per configured
    /// server on the first `load_tools("mcp.<server>")` reveal. `None` in
    /// tests / on MCP-free installs; set once at startup via
    /// [`with_mcp`](Self::with_mcp).
    mcp: Option<Arc<crate::mcp::McpManager>>,
    /// The runtime-mutable list of git subcommands that are *core operations*
    /// (always force the approval prompt, even in Autonomous mode or under a
    /// safety rule). Shared via `Arc<RwLock<...>>` with every `GitTool` built
    /// from this factory, so a `save_settings` call (which calls
    /// [`set_core_operations`](Self::set_core_operations)) is observed by
    /// every live `GitTool` on its next `never_auto_for` check — no registry
    /// rebuild needed. Defaults to `["merge", "push"]`.
    core_operations: Arc<RwLock<Vec<String>>>,
    /// The runtime-mutable shell-output filter config (`[shell_filter]`).
    /// Shared via `Arc<RwLock<...>>` with every `ShellTool` built from this
    /// factory, so a `save_settings` call (which calls
    /// [`set_shell_filter_config`](Self::set_shell_filter_config)) is observed
    /// by every live `ShellTool` on its next command — no registry rebuild
    /// needed. Defaults to enabled with no overrides.
    shell_filter: Arc<RwLock<ShellFilterConfig>>,
    /// Live mirror of `[general] enable_browser_inspection`, shared with every
    /// built registry's browser `Ctx`. Drives whether the six on-screen
    /// `browser_*` tools are advertised at all (see `Tool::available`) —
    /// atomic so a Settings save lands on the next turn without a rebuild.
    browser_inspection: Arc<std::sync::atomic::AtomicBool>,
}

impl AgentLoopFactory {
    /// Create a factory holding the shared, stateless deps.
    ///
    /// Every subsequent [`build`](Self::build) clones the shared deps and
    /// creates a fresh per-agent `Workflow` + `ToolRegistry`.
    pub fn new(
        provider: Arc<dyn LlmClient>,
        constitution_source: ConstitutionSource,
        memory: Option<Arc<dyn MemoryStoreTrait>>,
        sandbox: Arc<Sandbox>,
        project_root: PathBuf,
        safety_rules: Option<Arc<SafetyRules>>,
        safety_mode: Arc<RwLock<SafetyMode>>,
        context_manager: ContextManager,
        plans_dir: PathBuf,
        vision: Option<Arc<dyn ImageDescriber>>,
    ) -> Self {
        // Derive the fill rate from the context manager so it can be rebuilt
        // when the provider is swapped (its max context changes). The default
        // of 0.5 is a safety net for a degenerate zero max_tokens.
        let fill_rate = if context_manager.max_tokens() > 0 {
            context_manager.summarize_at() as f64 / context_manager.max_tokens() as f64
        } else {
            0.5
        };
        // Preserve the preflight settings so rebuilt ContextManagers (on model
        // swap) keep the same hard-ceiling guard behavior.
        let preflight_compact = context_manager.preflight_compact();
        let compact_headroom_tokens = context_manager.compact_headroom_tokens();
        // Preserve the proxy cache ceiling so rebuilt ContextManagers keep
        // the cliff guard (same preservation contract as preflight).
        let proxy_cache_ceiling = context_manager.proxy_cache_ceiling();
        Self {
            provider: RwLock::new(provider.clone()),
            default_display_effort: RwLock::new(None),
            context_manager: RwLock::new(context_manager),
            fill_rate,
            proxy_cache_ceiling,
            preflight_compact,
            compact_headroom_tokens,
            constitution_source,
            memory,
            // The knowledge dir is the plans dir's sibling (`.coding/knowledge`),
            // so a plans dir that implies no root yields no backing (the memory
            // tools then keep their historical DB-only behavior).
            knowledge: plans_dir
                .parent()
                .map(|p| p.join(knowledge::KNOWLEDGE_DIR_NAME))
                .map(KnowledgeStore::new)
                .map(Arc::new),
            sandbox,
            project_root,
            safety_rules,
            safety_mode,
            plans_dir,
            vision: SwappableVision::new(vision),
            // The provider slot starts populated with the initial provider so
            // the `memory_consolidate` tool can run full extraction on its
            // first call. `set_provider` keeps this slot in sync with swaps.
            provider_slot: SwappableProvider::new(Some(provider)),
            // No spawner at construction — the IPC layer wires it in via
            // `set_spawner` once the `AgentManager` exists. Until then the
            // `spawn_agent` tool is omitted from every registry.
            spawner: RwLock::new(None),
            // No descendant tracker at construction — the IPC layer wires it
            // in via `set_descendant_tracker` alongside the spawner. Until
            // then the state-transition gate is not enforced.
            descendant_tracker: RwLock::new(None),
            // No backlog store at construction — the IPC layer wires it in
            // via `set_backlog` once the shared store exists. Until then the
            // `backlog_add` tool is omitted from every registry.
            backlog: RwLock::new(None),
            // No skill registry at construction — the IPC layer wires it in
            // via `with_skills` once the project's skills dir is known.
            skills: None,
            // No model resolver at construction — the IPC layer wires it in
            // via `with_model_resolver` once the live config handle is known.
            model_resolver: None,
            // A fresh browser manager by default; the IPC layer swaps in the
            // shared one via `set_browser` so the UI + agent share a browser.
            #[cfg(feature = "browser")]
            browser: crate::browser::BrowserManager::new(),
            // No code graph at construction — the IPC layer wires it in via
            // `with_codegraph` once the project's DB is opened. Until then
            // the `graph_*` tools are omitted from every registry.
            codegraph: None,
            // Default core operations (merge/push); the IPC layer overrides
            // via `with_core_operations` from the loaded config at startup.
            core_operations: Arc::new(RwLock::new(vec!["merge".to_string(), "push".to_string()])),
            // Default shell filter (enabled, no overrides); the IPC layer
            // overrides via `with_shell_filter_config` from the loaded config
            // at startup.
            shell_filter: Arc::new(RwLock::new(ShellFilterConfig::default())),
            // Mirrors the config default (`enable_browser_inspection = false`)
            // until the IPC layer pushes the loaded value at startup.
            browser_inspection: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            mcp: None,
            // No auto-typing gate at construction — the IPC layer wires it
            // via `with_auto_typing` once the app runtime's classifier slot
            // exists (backlog a147b63c). Until then `memory_write` never
            // asks a classifier.
            typing: None,
        }
    }

    /// Wire in the shared [`McpManager`](crate::mcp::McpManager) (parsed
    /// from `mcp.toml`). Called once by the IPC layer at startup; affects
    /// agents built after this call. When not wired, `mcp.*` groups stay
    /// out of the deferred-group table entirely (MCP-free installs keep
    /// byte-identical prompts).
    pub fn with_mcp(mut self, mcp: Arc<crate::mcp::McpManager>) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// Wire the shared Laya auto-typing gate (backlog a147b63c): the
    /// `memory_write` tool reads the shared classifier slot + the
    /// `[general.laya] auto_type_memories` flag at call time. When not
    /// wired, `memory_write` keeps its pre-auto-typing behavior.
    pub fn with_auto_typing(mut self, handle: AutoTypingHandle) -> Self {
        self.typing = Some(handle);
        self
    }

    /// Flip the auto-typing enable flag on the shared gate — the Settings
    /// save path (rewire) calls this so the toggle reaches already-built
    /// tools with no registry rebuild.
    pub fn set_auto_typing_enabled(&self, on: bool) {
        if let Some(handle) = &self.typing {
            handle
                .enabled
                .store(on, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The shared MCP manager, when wired (the IPC layer's Settings/Test
    /// commands reuse it).
    pub fn mcp_manager(&self) -> Option<Arc<crate::mcp::McpManager>> {
        self.mcp.clone()
    }

    /// Wire in the [`SkillLibrary`] (the `.coding/skills/` dir + the registry
    /// loaded from it). Called once by the IPC layer at startup. Affects agents
    /// built **after** this call (the library is read per `build`), so the IPC
    /// layer sets it before building the main agent. When `None`, the skill
    /// tools are omitted (no skills available).
    pub fn with_skills(mut self, skills: Arc<SkillLibrary>) -> Self {
        self.skills = Some(skills);
        self
    }

    /// Wire in a per-context [`ModelResolver`]. Called once by the IPC layer
    /// at startup. Affects agents built **after** this call (the resolver is
    /// read per turn), so the IPC layer sets it before building the main agent.
    /// When `None`, every turn uses the default provider (the historical
    /// behavior — no `[models]` overrides).
    pub fn with_model_resolver(
        mut self,
        resolver: Arc<dyn crate::model_resolver::ModelResolver>,
    ) -> Self {
        self.model_resolver = Some(resolver);
        self
    }

    /// Wire in the shared, runtime-mutable git core-operations handle. Called
    /// once by the IPC layer at startup (from the loaded `[git]` config
    /// section). Every `GitTool` built **after** this call shares the same
    /// `Arc<RwLock<Vec<String>>>`, so a later `set_core_operations` (from a
    /// `save_settings` call) is observed live without a registry rebuild.
    pub fn with_core_operations(mut self, ops: Arc<RwLock<Vec<String>>>) -> Self {
        self.core_operations = ops;
        self
    }

    /// Wire in the shared, runtime-mutable shell-output filter handle. Called
    /// once by the IPC layer at startup (from the loaded `[shell_filter]`
    /// config section). Every `ShellTool` built **after** this call shares the
    /// same `Arc<RwLock<ShellFilterConfig>>`, so a later
    /// [`set_shell_filter_config`](Self::set_shell_filter_config) (from a
    /// `save_settings` call) is observed live without a registry rebuild.
    pub fn with_shell_filter_config(mut self, cfg: Arc<RwLock<ShellFilterConfig>>) -> Self {
        self.shell_filter = cfg;
        self
    }

    /// Wire in the [`AgentSpawner`] that backs the `spawn_agent` tool.
    ///
    /// Called once by the IPC layer after the `AgentManager` + factory exist.
    /// Only affects agents built **after** this call (the tool is read per
    /// `build`), so the IPC layer sets it before building the main agent.
    pub fn set_spawner(&self, spawner: Arc<dyn AgentSpawner>) {
        *self.spawner.write().expect("spawner lock poisoned") = Some(spawner);
    }

    /// Wire in the [`DescendantTracker`] that backs the workflow-state-
    /// transition gate (the agent cannot change state while spawned
    /// subagents are running).
    ///
    /// Called once by the IPC layer alongside [`set_spawner`](Self::set_spawner)
    /// (the concrete `IpcSpawner` implements both traits). Only affects agents
    /// built **after** this call, so the IPC layer sets it before building the
    /// main agent.
    pub fn set_descendant_tracker(&self, tracker: Arc<dyn crate::runtime::DescendantTracker>) {
        *self
            .descendant_tracker
            .write()
            .expect("descendant_tracker lock poisoned") = Some(tracker);
    }

    /// Wire in the shared backlog store that backs the `backlog_add` tool.
    ///
    /// Called once by the IPC layer at startup with the SAME
    /// `Arc<tokio::sync::Mutex<BacklogStore>>` the `backlog_*` commands use,
    /// so agent adds and UI mutations serialize through one mutex and can
    /// never clobber each other. The wiring slot also carries an optional UI
    /// notifier injected later via [`set_backlog_notifier`](Self::set_backlog_notifier)
    /// so agent adds emit `backlog://changed` live. Only affects agents built
    /// **after** this call (the tool is read per `build`), so the IPC layer
    /// sets it before building the main agent. Also omitted in console mode
    /// (`-console`): no store is opened there, so the tool is not registered
    /// (review finding 7).
    pub fn set_backlog(&self, store: Arc<tokio::sync::Mutex<crate::backlog::BacklogStore>>) {
        *self.backlog.write().expect("backlog lock poisoned") = Some(BacklogWiring {
            store,
            on_changed: None,
        });
    }

    /// Wire the UI notifier into the backlog wiring: after every successful
    /// agent-side `backlog_add`, the callback fires so the app layer can
    /// emit `backlog://changed` and the Backlog tab updates live. No-op when
    /// no store is wired (the IPC layer calls [`set_backlog`](Self::set_backlog)
    /// first, before this).
    pub fn set_backlog_notifier(&self, on_changed: Arc<dyn Fn() + Send + Sync>) {
        let mut slot = self.backlog.write().expect("backlog lock poisoned");
        if let Some(wiring) = slot.as_mut() {
            wiring.on_changed = Some(on_changed);
        }
    }

    /// Update the live git core-operations list (called by `save_settings`
    /// after a `[git]` config patch is persisted). Every `GitTool` built from
    /// this factory shares the same `Arc<RwLock<Vec<String>>>`, so this
    /// mutation is observed on each tool's next `never_auto_for` check — no
    /// registry rebuild needed.
    pub fn set_core_operations(&self, ops: Vec<String>) {
        *self
            .core_operations
            .write()
            .expect("core_operations lock poisoned") = ops;
    }

    /// Update the live shell-output filter config (called by `save_settings`
    /// after the `[shell_filter]` config is persisted/reloaded). Every
    /// `ShellTool` built from this factory shares the same
    /// `Arc<RwLock<ShellFilterConfig>>`, so this mutation is observed on each
    /// tool's next command — no registry rebuild needed.
    pub fn set_shell_filter_config(&self, cfg: ShellFilterConfig) {
        *self
            .shell_filter
            .write()
            .expect("shell_filter lock poisoned") = cfg;
    }

    /// Swap in a new provider (e.g. when the user switches models from the
    /// status bar). Rebuilds the context manager from the new provider's max
    /// context so freshly built agents summarize at the right threshold.
    ///
    /// Live agents already built from this factory are NOT updated here — the
    /// IPC layer swaps each live loop separately via [`AgentLoop::set_provider`].
    pub fn set_provider(&self, provider: Arc<dyn LlmClient>) {
        let max_context = provider.capabilities().max_context;
        *self.provider.write().expect("provider lock poisoned") = provider.clone();
        // Keep the shared slot in sync so LLM-backed tools
        // (memory_consolidate) see the swap on their next call without a
        // registry rebuild.
        self.provider_slot.set(Some(provider));
        *self
            .context_manager
            .write()
            .expect("context_manager lock poisoned") =
            ContextManager::new(max_context, self.fill_rate)
                .with_preflight(self.preflight_compact, self.compact_headroom_tokens)
                .with_proxy_cache_ceiling(self.proxy_cache_ceiling);
    }

    /// Record the DISPLAY-space effort of the shared default provider slot
    /// (call alongside [`set_provider`](Self::set_provider) — the swap
    /// callers know the new default's effort). Stamped onto every loop built
    /// after this point; live loops are updated separately by the IPC swap
    /// path. `None` = unknown (the UI falls back).
    pub fn set_default_display_effort(&self, effort: Option<String>) {
        *self
            .default_display_effort
            .write()
            .expect("default_display_effort lock poisoned") = effort;
    }

    /// The DISPLAY-space effort recorded for the shared default provider
    /// slot (see [`set_default_display_effort`](Self::set_default_display_effort)).
    pub fn default_display_effort(&self) -> Option<String> {
        self.default_display_effort
            .read()
            .expect("default_display_effort lock poisoned")
            .clone()
    }

    /// Replace the vision client at runtime (Settings → Vision, or after an
    /// endpoint/key edit that the vision model uses). Shared with every live
    /// agent loop and the `describe_image` tool via [`SwappableVision`].
    pub fn set_vision(&self, vision: Option<Arc<dyn ImageDescriber>>) {
        self.vision.set(vision);
    }

    /// Push `[general] enable_browser_inspection` into the shared flag every
    /// registry's browser `Ctx` reads. Called by the IPC layer at startup and
    /// again on every Settings save, so hiding/showing the on-screen
    /// `browser_*` tools takes effect on the next turn.
    pub fn set_browser_inspection(&self, enabled: bool) {
        self.browser_inspection
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the on-screen `browser_*` tools are currently advertised.
    pub fn browser_inspection_enabled(&self) -> bool {
        self.browser_inspection
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Shared vision slot (for the IPC layer / diagnostics).
    pub fn vision_slot(&self) -> Arc<SwappableVision> {
        Arc::clone(&self.vision)
    }

    /// Inject the shared browser manager (the IPC layer's copy, so the
    /// right-panel Browser tab and the agent drive the same browser). Builder
    /// style like [`with_skills`](Self::with_skills) — call before wrapping the
    /// factory in an `Arc`. When not called, a fresh manager is used (tests).
    /// Only exists under the `browser` feature.
    #[cfg(feature = "browser")]
    pub fn with_browser(mut self, browser: Arc<crate::browser::BrowserManager>) -> Self {
        self.browser = browser;
        self
    }

    /// The shared browser manager (for the IPC layer to register its
    /// commands). Only exists under the `browser` feature.
    #[cfg(feature = "browser")]
    pub fn browser_slot(&self) -> Arc<crate::browser::BrowserManager> {
        Arc::clone(&self.browser)
    }

    /// Wire in the per-project code knowledge graph. Builder style like
    /// `with_browser` — call before wrapping the factory
    /// in an `Arc`. Affects agents built **after** this call (the graph is
    /// read per `build`), so the IPC layer sets it before building the main
    /// agent. When `None` (the default), the `graph_*` tools are omitted.
    pub fn with_codegraph(mut self, graph: Arc<crate::codegraph::CodeGraph>) -> Self {
        self.codegraph = Some(graph);
        self
    }

    /// The shared code knowledge graph, if wired (for the IPC layer's
    /// `codegraph_*` commands and tests).
    pub fn codegraph_handle(&self) -> Option<Arc<crate::codegraph::CodeGraph>> {
        self.codegraph.clone()
    }

    /// Build a context manager sized to the given provider, using the factory's
    /// fill rate. Used by the IPC layer when swapping a live agent's provider.
    pub fn context_manager_for(&self, provider: &dyn LlmClient) -> ContextManager {
        ContextManager::new(provider.capabilities().max_context, self.fill_rate)
            .with_preflight(self.preflight_compact, self.compact_headroom_tokens)
            .with_proxy_cache_ceiling(self.proxy_cache_ceiling)
    }

    /// A handle to the shared memory store (if any). The IPC layer uses this
    /// to query per-session + per-project stats (the stats live in the memory
    /// store's SQLite DB at `.coding/memory.db`).
    pub fn memory_handle(&self) -> Option<Arc<dyn MemoryStoreTrait>> {
        self.memory.clone()
    }

    /// A handle to the shared skill library (if any). The IPC layer's
    /// `enter_skill` command uses this to validate + look up a skill when the
    /// UI button starts a skill.
    pub fn skills_handle(&self) -> Option<Arc<SkillLibrary>> {
        self.skills.clone()
    }

    /// The proxy cache ceiling the factory's context managers are built
    /// with — the startup snapshot (backlog ffd4bac3): the run-all
    /// between-items auto-compact gate reads it so its threshold uses the
    /// SAME ceiling the engine's fill-rate path applies, even after a
    /// mid-session settings edit (the config file may be newer than the
    /// factory until restart).
    pub fn proxy_cache_ceiling(&self) -> Option<usize> {
        self.proxy_cache_ceiling
    }

    /// Build a fresh `AgentLoop` with its own `Workflow` + `ToolRegistry`,
    /// without assigning it a runtime id (the `spawn_agent` tool then spawns
    /// parentless agents). Used by tests and by callers that don't register the
    /// agent with a manager. Production spawns should use
    /// [`build_with_id`](Self::build_with_id).
    pub fn build(&self) -> Arc<AgentLoop> {
        self.build_inner(None, self.plans_dir.clone(), None)
    }

    /// Build a fresh `AgentLoop` and assign it the given runtime `AgentId`.
    ///
    /// The id is set on the loop (so the agent knows itself) and passed to the
    /// `spawn_agent` tool as the parent id for any background agents it
    /// spawns — this is what closes the completion-notification feedback loop.
    /// The caller allocates the id from the `AgentManager` so ids stay unique.
    pub fn build_with_id(&self, id: crate::runtime::AgentId) -> Arc<AgentLoop> {
        self.build_inner(Some(id), self.plans_dir.clone(), None)
    }

    /// Build a fresh `AgentLoop` with a per-agent plans dir override.
    ///
    /// Used for UI-spawned parentless agents so each gets its own
    /// `.coding/plans/agents/<id>/` dir (the main agent keeps `.coding/plans/`).
    /// The override flows ONLY into the `Workflow` + the loop's `plans_dir`
    /// (for session-end consolidation); `reviews_dir()` keeps using the main
    /// `plans_dir` so reviewer reports always land at `.coding/reviews/`.
    /// Subagents (tool-spawned, with a parent) should NOT use this — they
    /// share the main dir and can't mutate plans.
    pub fn build_with_id_and_plans_dir(
        &self,
        id: crate::runtime::AgentId,
        plans_dir: PathBuf,
    ) -> Arc<AgentLoop> {
        self.build_inner(Some(id), plans_dir, None)
    }

    /// Build a fresh `AgentLoop` bound to a per-agent root spec (parallel
    /// run-all worktree agents, plan ffd7a86f).
    ///
    /// The agent's file/shell/search tools, git tools, and code graph all
    /// operate on the spec's worktree root instead of the factory's project
    /// root; everything else (memory, backlog wiring, skills, provider,
    /// spawner, reviews) stays factory-shared. `plans_dir` should be inside
    /// the worktree (e.g. `<worktree>/.coding/plans/agents/<id>/`) so the
    /// agent's plan bookkeeping lands on its own branch. The spec is stored
    /// on the loop so subagents spawned by this agent inherit the same
    /// root (a worktree agent's reviewer must see the worktree's diff).
    pub fn build_with_root_spec(
        &self,
        id: crate::runtime::AgentId,
        plans_dir: PathBuf,
        root: AgentRootSpec,
    ) -> Arc<AgentLoop> {
        self.build_inner(Some(id), plans_dir, Some(&root))
    }

    /// The main plans dir (`.coding/plans/`), used to derive the reviews dir.
    pub fn plans_dir(&self) -> &Path {
        &self.plans_dir
    }

    /// Shared build logic. `agent_id` is `Some` for manager-registered agents
    /// (their `spawn_agent` tool becomes parent-aware) and `None` otherwise.
    /// `plans_dir` is the dir the agent's `Workflow` + loop use (the main dir
    /// or a per-agent override).
    fn build_inner(
        &self,
        agent_id: Option<crate::runtime::AgentId>,
        plans_dir: PathBuf,
        root: Option<&AgentRootSpec>,
    ) -> Arc<AgentLoop> {
        // Fresh per-agent workflow — load the latest plan from disk so a
        // newly-spawned agent resumes an in-flight plan. The `let _ =` is
        // intentional: a fresh/empty plans dir legitimately has no plan to
        // load (Ok path is a no-op, Err just means "start in Planning"), so
        // there's nothing to act on here.
        //
        // Parented subagents (spawn.rs, `parent_id.is_some()`) share the MAIN
        // plans dir, so this load populates their plan stack from the main
        // plan — the read-only mirror that feeds `current_plan` for reviewers
        // and the UI staircase. The DERIVED lifecycle state (Executing/
        // Reviewing/…) is then overwritten by the spawn path via
        // `enter_subagent_state()`: a sub-agent is a single-task worker, not
        // a plan-lifecycle owner, and no behavioral gate may key on a state
        // it never owned.
        let workflow = Arc::new(Mutex::new({
            let mut wf = Workflow::new(plans_dir.clone());
            let _ = wf.load_latest();
            wf
        }));

        // Fresh per-agent tool registry, wired to this agent's workflow and
        // stamped with this agent's id (for the parent-aware spawn_agent tool).
        let registry = self.build_registry(&workflow, agent_id, root, &plans_dir);

        // Snapshot the shared provider + context manager (swappable at
        // runtime). The locks are released immediately (cheap clones).
        let provider = self
            .provider
            .read()
            .expect("provider lock poisoned")
            .clone();
        let context_manager = self
            .context_manager
            .read()
            .expect("context_manager lock poisoned")
            .clone();

        let mut agent = AgentLoop::with_constitution_source(
            AgentLoopConfig {
                provider,
                tools: Arc::new(registry),
                workflow,
                sandbox: match root {
                    Some(spec) => Arc::new(spec.sandbox.clone()),
                    None => Arc::clone(&self.sandbox),
                },
                // The initial safety mode is read from the shared handle so a
                // runtime toggle (by another agent or the UI) is reflected. The
                // handle itself is shared, so all agents see the same mode.
                safety_mode: *self.safety_mode.read().expect("safety_mode lock poisoned"),
                context_manager,
                memory: self.memory.clone(),
                // Share the swappable vision slot so Settings rewires live agents.
                vision: Some(self.vision.clone() as Arc<dyn ImageDescriber>),
            },
            self.constitution_source.clone(),
        )
        .with_safety_mode_handle(Arc::clone(&self.safety_mode))
        // Wire the plans dir so session-end consolidation can digest the
        // accumulated plan/review corpus (learning material). Uses the
        // per-build override (main dir or per-agent dir).
        .with_plans_dir(plans_dir);

        if let Some(id) = agent_id {
            agent = agent.with_agent_id(id);
        }

        if let Some(rules) = &self.safety_rules {
            agent = agent.with_safety_rules(Arc::clone(rules));
        }

        // Wire the per-context model resolver (if any) + the fill rate so a
        // resolved per-context model can build a correctly-sized context
        // manager. The resolver is shared (reads the live config per turn), so
        // a Settings save takes effect on the next turn without a rebuild.
        if let Some(resolver) = &self.model_resolver {
            agent = agent
                .with_model_resolver(Arc::clone(resolver))
                .with_fill_rate(self.fill_rate);
        }

        // Wire the descendant tracker (if any) so the dispatch layer can gate
        // workflow-state transitions on "no spawned subagents running".
        let dt = self
            .descendant_tracker
            .read()
            .expect("descendant_tracker lock poisoned")
            .clone();
        if let Some(tracker) = dt {
            agent = agent.with_descendant_tracker(tracker);
        }

        // Wire the code graph so the dispatch-layer symbol-lookup redirect
        // gate (C5) can resolve symbol-shaped search patterns to their graph
        // ids before the tool runs. `None` when indexing is disabled — the
        // gate is a no-op.
        // A root-spec agent gets the spec's graph (the worktree's), NOT the
        // factory's — even when the spec's is None (indexing disabled for
        // that worktree must not silently fall back to the main tree's
        // graph).
        agent = agent.with_codegraph(match root {
            Some(spec) => spec.codegraph.clone(),
            None => self.codegraph.clone(),
        });

        // Store the root spec on the loop so subagents spawned by this
        // agent inherit the same root (spawn_agent_shared reads it).
        agent = agent.with_root_spec(root.cloned());

        // Stamp the shared default's DISPLAY effort (backlog 51dab4da): the
        // loop's no-override resolution branch reports it, so the status bar
        // shows the default model's effective effort (per-model override →
        // endpoint default → "max") — never just the endpoint-level default.
        // None for test factories (the UI falls back).
        agent.set_default_display_effort(self.default_display_effort());

        Arc::new(agent)
    }

    /// Build a `ToolRegistry` wired to the given (per-agent) workflow, stamped
    /// with this agent's runtime id (so the `spawn_agent` tool becomes
    /// parent-aware). `agent_id` is `None` for id-less builds (tests).
    ///
    /// Registrations are grouped by subsystem (Maint M5) so the tool set is
    /// easy to audit at a glance: agent/file tools, workflow/plan tools, skill
    /// tools, the vision tool, memory tools, and the spawn tool. The exact set
    /// + conditionals (skills/spawner/memory are optional) are unchanged.
    fn build_registry(
        &self,
        workflow: &Arc<Mutex<Workflow>>,
        agent_id: Option<crate::runtime::AgentId>,
        root: Option<&AgentRootSpec>,
        plans_dir: &Path,
    ) -> ToolRegistry {
        // The sandbox is Clone (just a PathBuf) — clone the inner value so
        // each tool gets its own owned copy, matching the tool constructors'
        // `Sandbox` (by-value) signature. A root-spec agent uses the spec's
        // worktree sandbox instead of the factory's project sandbox.
        let sandbox = match root {
            Some(spec) => spec.sandbox.clone(),
            None => (*self.sandbox).clone(),
        };
        // Each agent gets its OWN reveal-set: a sub-agent that loads the
        // browser group must not silently enlarge its parent's tools array.
        // The deferred-group table adds one mcp.<server> entry per ENABLED
        // configured MCP server (an MCP-free install = the static base,
        // byte-identical prompts).
        let mcp_table = match &self.mcp {
            Some(manager) => crate::tool::deferred_groups_with(&manager.servers()),
            None => crate::tool::deferred_groups_with(&[]),
        };
        let mut registry =
            ToolRegistry::with_groups(crate::tool::LoadedGroups::new(), mcp_table.clone());
        self.register_agent_tools(&mut registry, &sandbox, root);
        self.register_workflow_tools(&mut registry, workflow, root, plans_dir);
        self.register_skill_tools(&mut registry, workflow, &sandbox);
        self.register_vision_tool(&mut registry, &sandbox);
        self.register_memory_tools(&mut registry, root);
        self.register_codegraph_tools(&mut registry, root);
        self.register_spawn_tool(&mut registry, agent_id);
        #[cfg(feature = "browser")]
        self.register_browser_tools(&mut registry, &sandbox);
        // The reveal half of progressive disclosure — registered last so it
        // shares the reveal-set the registry was built with. MCP groups
        // reveal through the shared manager into this registry's dynamic
        // tool slot (the same shared-handle pattern as the reveal-set).
        let mcp_reveal = self.mcp.as_ref().map(|manager| {
            crate::mcp::McpReveal::new(std::sync::Arc::clone(manager), registry.dynamic_slot())
        });
        // The static deferred tools' schemas render in the reveal response —
        // snapshot them (Arc clones) before LoadToolsTool registers; static
        // tools never change at runtime.
        let deferred = Self::deferred_snapshot(&registry);
        registry.register(Box::new(
            crate::tool::agent::load_tools::LoadToolsTool::new(
                registry.loaded_groups(),
                mcp_table,
                mcp_reveal,
                deferred,
            ),
        ));
        registry
    }

    /// The reviews directory (`.coding/reviews/`), derived from `plans_dir`
    /// the same way `FinishTool` derives it at runtime
    /// (`plans_dir.parent().join("reviews")`). Used to construct the
    /// `write_review_report` tool so the reviewer subagent's single output
    /// channel writes to the same dir `finish` later validates.
    fn reviews_dir(&self) -> std::path::PathBuf {
        self.plans_dir
            .parent()
            .map(|p| p.join("reviews"))
            .unwrap_or_else(|| std::path::PathBuf::from(".coding/reviews"))
    }

    /// Register the file/shell/search/git agent tools (always present). These
    /// are the coding tools the agent uses to do work; they are gated by the
    /// workflow's `ToolFilter` at dispatch time, not at registration.
    fn register_agent_tools(
        &self,
        registry: &mut ToolRegistry,
        sandbox: &Sandbox,
        root: Option<&AgentRootSpec>,
    ) {
        // A root-spec agent's graph/git bindings come from the spec (the
        // worktree's), not the factory's — even when the spec's graph is
        // None (indexing disabled for that worktree must not silently fall
        // back to the main tree's graph).
        let codegraph = match root {
            Some(spec) => spec.codegraph.clone(),
            None => self.codegraph.clone(),
        };
        let project_root = match root {
            Some(spec) => spec.project_root.clone(),
            None => self.project_root.clone(),
        };
        registry.register(Box::new(
            ReadFilesTool::new(sandbox.clone()).with_codegraph(codegraph.clone()),
        ));
        registry.register(Box::new(FileEditTool::new(sandbox.clone())));
        registry.register(Box::new(MultiEditTool::new(sandbox.clone())));
        registry.register(Box::new(FileWriteTool::new(sandbox.clone())));
        registry.register(Box::new(ConvertLineEndingsTool::new(sandbox.clone())));
        registry.register(Box::new(
            ShellTool::new(sandbox.clone())
                .with_filter_config(Arc::clone(&self.shell_filter))
                // Same handle as GitTool: `shell git merge|push` must raise the
                // always-on core-operation prompt too (follow-up A, 2027-01-11).
                .with_core_operations(Arc::clone(&self.core_operations)),
        ));
        let mut search_tool = SearchTool::new(sandbox.clone(), codegraph.clone());
        let mut search_read_tool = SearchReadTool::new(sandbox.clone(), codegraph.clone());
        // F9: the memory store powers the known-memory-hit note on
        // uuid-shaped search patterns.
        if let Some(store) = &self.memory {
            search_tool = search_tool.with_memory(store.clone());
            search_read_tool = search_read_tool.with_memory(store.clone());
        }
        registry.register(Box::new(search_tool));
        registry.register(Box::new(search_read_tool));
        registry.register(Box::new(WebFetchTool::new()));
        registry.register(Box::new(GitTool::with_core_operations(
            &project_root,
            self.core_operations.clone(),
        )));
        // git_read is the read-only view into git (op = diff | log | show | status):
        // the reviewer subagent's only way to see uncommitted changes (it has
        // no shell/git), and the bridge from a memory record's commit pointer
        // to shipped code. AutoRun by construction, which is exactly why it
        // stays separate from the NeedsApproval `git` tool — it must remain
        // visible in the read-only Planning/Complete states.
        registry.register(Box::new(GitReadTool::new(&project_root)));
        // write_review_report is the reviewer subagent's single output channel
        // (writes only under .coding/reviews/). Always registered; gated by
        // the workflow ToolFilter.
        registry.register(Box::new(WriteReviewReportTool::new(self.reviews_dir())));
        // list_models is read-only discovery (which models are configured) so
        // the agent can pick one for spawn_agent's `model` parameter. Omitted
        // when no model resolver is wired (tests / no `[models]` section) —
        // there's nothing to list.
        if let Some(resolver) = &self.model_resolver {
            registry.register(Box::new(ListModelsTool::new(Some(Arc::clone(resolver)))));
        }
    }

    /// Register the plan-workflow tools (always present). These mutate the
    /// per-agent `Workflow` (create/complete/update/abandon a plan) and write
    /// the project's own `.coding/plans/` bookkeeping.
    fn register_workflow_tools(
        &self,
        registry: &mut ToolRegistry,
        workflow: &Arc<Mutex<Workflow>>,
        root: Option<&AgentRootSpec>,
        plans_dir: &Path,
    ) {
        let mut create = CreatePlanTool::new(workflow.clone());
        if let Some(store) = &self.memory {
            create = create.with_memory(store.clone());
        }
        registry.register(Box::new(create));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        registry.register(Box::new(UpdatePlanTool::new(workflow.clone())));
        // abandon_plan supersedes the abandoned plan's lingering crash
        // markers when the memory store is wired (Phase 4 hygiene — mirrors
        // finish's marker supersede; see AbandonPlanTool::with_memory).
        let mut abandon = AbandonPlanTool::new(workflow.clone());
        if let Some(store) = &self.memory {
            abandon = abandon.with_memory(store.clone());
        }
        registry.register(Box::new(abandon));
        // finish is the only path from Reviewing → Complete, gated on a review
        // report under .coding/reviews/ (the closing sequence's exit gate).
        // The reviews dir is the factory's main-derived path (NOT the per-agent
        // workflow's plans_dir) so a per-agent plans dir doesn't redirect it.
        // Phase 4: the finish hook also auto-captures the PLAN:/BUG: digests
        // (memory) and gates bug_fixing regression tests on the code graph.
        let mut finish = FinishTool::new(workflow.clone(), self.reviews_dir());
        if let Some(store) = &self.memory {
            finish = finish.with_memory(store.clone());
        }
        // The knowledge-file backing: the BUG: capture lands in a knowledge
        // file (the truth) instead of an authored row — same store-derived
        // path as the memory tools' backing.
        if let Some(knowledge) = &self.knowledge {
            finish = finish.with_knowledge(knowledge.clone());
        }
        // A root-spec agent's finish gate validates regression tests against
        // the worktree's graph (the spec's), not the factory's.
        let finish_graph = match root {
            Some(spec) => spec.codegraph.clone(),
            None => self.codegraph.clone(),
        };
        if let Some(graph) = &finish_graph {
            finish = finish.with_codegraph(graph.clone());
        }
        registry.register(Box::new(finish));
        // current_plan is a read-only query (available in every state, like
        // ask_user + memory tools) so the agent can recover when disoriented
        // about which stacked plan is active.
        // current_plan reads the plan file for THIS agent's workflow stack —
        // the build's plans dir (main dir, per-agent dir, or worktree dir),
        // not the factory's main dir (a worktree agent must see its own
        // plans, not the main agent's).
        registry.register(Box::new(CurrentPlanTool::new(
            workflow.clone(),
            plans_dir.to_path_buf(),
        )));
        // ask_user is always available (not gated by ToolFilter — like memory
        // tools) so the agent can ask a question in any workflow state. The
        // pause is driven from dispatch, not the tool's execute().
        registry.register(Box::new(AskUserTool::new()));
        // backlog_add shares the app's one store handle (the same Arc the
        // IPC backlog commands lock) — the sanctioned agent write path to
        // the user's backlog (.coding/backlog.jsonl is protected from the
        // file tools). The optional on_changed notifier (wired by the IPC
        // layer via set_backlog_notifier) fires after each add so the
        // Backlog tab updates live. Omitted entirely when no store is
        // wired (tests / NeedsProject).
        if let Some(wiring) = self.backlog.read().expect("backlog lock poisoned").as_ref() {
            registry.register(Box::new(
                crate::tool::workflow::backlog::BacklogAddTool::new(
                    Arc::clone(&wiring.store),
                    wiring.on_changed.clone(),
                ),
            ));
            // backlog_status — same wiring; changes status through the same
            // guarded transition table the harness uses (see the tool docs).
            registry.register(Box::new(
                crate::tool::workflow::backlog::BacklogStatusTool::new(
                    Arc::clone(&wiring.store),
                    wiring.on_changed.clone(),
                ),
            ));
            // backlog_list — read-only query; needs no notifier (it never
            // mutates). Safe for read-only reviewers (their strict allow-list
            // names it): querying the backlog is allowed, modifying it is not.
            registry.register(Box::new(
                crate::tool::workflow::backlog::BacklogListTool::new(Arc::clone(&wiring.store)),
            ));
        }
    }

    /// Register the skill tools — `skill_start` (gated to Complete + Planning
    /// by ToolFilter), `skill_end` + `abandon_skill` (only meaningful while a
    /// skill is active; exposed by the Skill filter unconditionally),
    /// `skill_reload` (every workflow state, an active skill included) and
    /// `skill_create` (Executing only). All AutoRun — protection is on the
    /// operations inside the skill (e.g. git merge/push are never_auto_for),
    /// not the entry or the authoring. Omitted entirely when no skill library
    /// is configured (tests).
    fn register_skill_tools(
        &self,
        registry: &mut ToolRegistry,
        workflow: &Arc<Mutex<Workflow>>,
        sandbox: &Sandbox,
    ) {
        if let Some(skills) = &self.skills {
            registry.register(Box::new(SkillStartTool::new(
                workflow.clone(),
                Arc::clone(skills),
            )));
            registry.register(Box::new(SkillEndTool::new(workflow.clone())));
            registry.register(Box::new(AbandonSkillTool::new(workflow.clone())));
            registry.register(Box::new(SkillReloadTool::new(Arc::clone(skills))));
            // The authoring tool gets the AGENT's sandbox (a root-spec agent
            // gets its worktree's), so skill_create obeys the same path policy
            // the file tools do: the link-free creation ladder + the `.coding/`
            // hardlink guard. A worktree agent therefore cannot author into
            // another tree's skills dir — by design, mirroring every other
            // write (review HIGH 1, 2027-01-16).
            registry.register(Box::new(SkillCreateTool::new(
                Arc::clone(skills),
                sandbox.clone(),
            )));
        }
    }

    /// Register the headless-browser tools (always present when the `browser`
    /// feature is selected). They share the
    /// factory's `BrowserManager` (the same instance the IPC layer injects via
    /// `with_browser`), so the agent and the right-panel Browser tab drive one
    /// browser. Reads are AutoRun; mutations (navigate/close/eval/click/type)
    /// are NeedsApproval — visible in every workflow state, gated per call by
    /// their approval prompt.
    #[cfg(feature = "browser")]
    fn register_browser_tools(&self, registry: &mut ToolRegistry, sandbox: &Sandbox) {
        use crate::tool::browser as bt;
        let ctx = || bt::Ctx {
            manager: Arc::clone(&self.browser),
            sandbox: sandbox.clone(),
            inspection: Arc::clone(&self.browser_inspection),
        };
        registry.register(Box::new(bt::OffscreenNavigateTool(ctx())));
        registry.register(Box::new(bt::OffscreenListPagesTool(ctx())));
        registry.register(Box::new(bt::OffscreenClosePageTool(ctx())));
        registry.register(Box::new(bt::OffscreenScreenshotTool(ctx())));
        registry.register(Box::new(bt::OffscreenConsoleTool(ctx())));
        registry.register(Box::new(bt::OffscreenSnapshotTool(ctx())));
        registry.register(Box::new(bt::OffscreenEvalTool(ctx())));
        registry.register(Box::new(bt::OffscreenClickTool(ctx())));
        registry.register(Box::new(bt::OffscreenTypeTool(ctx())));
        registry.register(Box::new(bt::OffscreenSwitchPageTool(ctx())));
        // Live WebView2 (Browser tab) inspection + control tools — attach to
        // the app's own WebView2 via CDP to inspect/drive the interactive
        // tab the human sees. browser_screenshot/browser_snapshot are
        // read-only (AutoRun); browser_eval/browser_navigate/browser_click/
        // browser_type run arbitrary JS / mutate the visible tab
        // (NeedsApproval).
        //
        // Windows-only: the child webview is a WebView2 instance and these
        // tools attach via its CDP debug endpoint — neither exists on other
        // platforms, so they are not registered there (the Browser tab shows
        // an explanatory panel instead; see ipc::browser_webview).
        #[cfg(windows)]
        {
            registry.register(Box::new(bt::BrowserScreenshotTool(ctx())));
            registry.register(Box::new(bt::BrowserEvalTool(ctx())));
            registry.register(Box::new(bt::BrowserSnapshotTool(ctx())));
            registry.register(Box::new(bt::BrowserNavigateTool(ctx())));
            registry.register(Box::new(bt::BrowserClickTool(ctx())));
            registry.register(Box::new(bt::BrowserTypeTool(ctx())));
        }
    }

    /// The static deferred tools whose schemas the reveal response renders —
    /// Arc clones snapshot at construction (static tools never change at
    /// runtime). Captured AFTER every register_* call: a registration that
    /// drifts past this point silently drops from the reveal response (the
    /// empty-block failure mode the snapshot test pins).
    fn deferred_snapshot(
        registry: &ToolRegistry,
    ) -> Vec<std::sync::Arc<dyn crate::tool::Tool>> {
        registry
            .iter()
            .filter(|t| t.deferred_group().is_some())
            .collect()
    }

    /// Register the image_* vision tools always, against the shared swappable
    /// slot. When the slot is empty, execute returns a clear "not configured"
    /// error — Settings can enable vision without rebuilding registries.
    fn register_vision_tool(&self, registry: &mut ToolRegistry, sandbox: &Sandbox) {
        let vision = self.vision.clone() as Arc<dyn ImageDescriber>;
        registry.register(Box::new(ImageUiToArtifactTool::new(
            sandbox.clone(),
            vision.clone(),
        )));
        registry.register(Box::new(ImageExtractTextTool::new(
            sandbox.clone(),
            vision.clone(),
        )));
        registry.register(Box::new(ImageDiagnoseErrorTool::new(
            sandbox.clone(),
            vision.clone(),
        )));
        registry.register(Box::new(ImageUnderstandDiagramTool::new(
            sandbox.clone(),
            vision.clone(),
        )));
        registry.register(Box::new(ImageAnalyzeChartTool::new(
            sandbox.clone(),
            vision.clone(),
        )));
        registry.register(Box::new(ImageUiDiffTool::new(
            sandbox.clone(),
            vision.clone(),
        )));
        registry.register(Box::new(ImageAnalysisTool::new(sandbox.clone(), vision)));
    }

    /// Register the memory tools when a memory store is configured. Omitted
    /// in tests that build a factory without memory.
    fn register_memory_tools(&self, registry: &mut ToolRegistry, root: Option<&AgentRootSpec>) {
        if let Some(store) = &self.memory {
            // The knowledge-file backing (typed records as files): typed
            // writes land in files (the truth) + the derived rows come from
            // reindexing. `None` (a plans dir that implies no root) keeps
            // the historical DB-only behavior.
            let knowledge = self.knowledge.clone();
            let typing = self.typing.clone();
            let mk_write = |store: Arc<dyn MemoryStoreTrait>| {
                let tool = match &knowledge {
                    Some(k) => MemoryWriteTool::with_knowledge(
                        store.clone(),
                        k.clone(),
                        self.plans_dir.clone(),
                    )
                    // A worktree lane agent shares the factory-wide knowledge
                    // store (main tree) — the tool's result says so explicitly.
                    .with_agent_root(root.map(|r| r.project_root.clone())),
                    None => MemoryWriteTool::new(store.clone()),
                };
                // The Laya auto-typing gate rides every memory_write —
                // call-time reads keep Settings swaps/toggles live.
                match &typing {
                    Some(handle) => tool.with_auto_typing(handle.clone()),
                    None => tool,
                }
            };
            registry.register(Box::new(mk_write(store.clone())));
            // The single read path: search or browse, any record type, any
            // tier. Replaced memory_recall / memory_list / plans_search /
            // reviews_search / past_fixes / context_pack, which differed only
            // by parameter values.
            registry.register(Box::new(MemorySearchTool::new(store.clone())));
            // Wire the swappable provider so manual consolidation runs the full
            // extraction pipeline (semantic + procedural), matching the
            // automatic session-end path. The slot is shared with the factory,
            // so a Settings model swap takes effect on the next call. The
            // plans dir feeds the plan/review corpus digest.
            registry.register(Box::new(MemoryConsolidateTool::with_provider(
                store.clone(),
                Arc::clone(&self.provider_slot),
                self.plans_dir.clone(),
            )));
            // Hygiene tools (Phase 1): in-place refine, supersede-not-delete,
            // junk delete, and query-less browsing.
            match &knowledge {
                Some(k) => {
                    registry.register(Box::new(MemoryUpdateTool::with_knowledge(
                        store.clone(),
                        k.clone(),
                        self.plans_dir.clone(),
                    )));
                    registry.register(Box::new(MemorySupersedeTool::with_knowledge(
                        store.clone(),
                        k.clone(),
                        self.plans_dir.clone(),
                    )));
                    registry.register(Box::new(MemoryDeleteTool::with_knowledge(
                        store.clone(),
                        k.clone(),
                        self.plans_dir.clone(),
                    )));
                    // memory_amend is file-only (it appends to the knowledge
                    // FILE) — registered only when the backing is wired.
                    registry.register(Box::new(MemoryAmendTool::new(
                        store.clone(),
                        k.clone(),
                        self.plans_dir.clone(),
                    )));
                }
                None => {
                    registry.register(Box::new(MemoryUpdateTool::new(store.clone())));
                    registry.register(Box::new(MemorySupersedeTool::new(store.clone())));
                    registry.register(Box::new(MemoryDeleteTool::new(store.clone())));
                    // NOTE (review L5): memory_amend is deliberately NOT
                    // registered here — it is file-only by construction and
                    // this branch has no knowledge-file backing. The compiled
                    // prompt names memory_amend unconditionally, so in this
                    // degraded (historical DB-only) config the model may hit
                    // a clean "unknown tool" error — accepted deliberately;
                    // the error is self-explanatory and the config is legacy.
                }
            }
        }
    }

    /// Register the CodeGraph tools when a graph is wired in (via
    /// [`with_codegraph`](Self::with_codegraph)). Omitted when the graph is
    /// disabled or failed to open — the tools don't exist in the registry
    /// rather than erroring at call time (same pattern as memory/spawn).
    fn register_codegraph_tools(
        &self,
        registry: &mut ToolRegistry,
        root: Option<&AgentRootSpec>,
    ) {
        // A root-spec agent's graph_* tools bind to the worktree's graph.
        let graph = match root {
            Some(spec) => spec.codegraph.clone(),
            None => self.codegraph.clone(),
        };
        if let Some(graph) = &graph {
            use crate::tool::agent::codegraph::{
                GraphContextTool, GraphImpactTool, GraphPathTool, GraphSearchTool,
            };
            registry.register(Box::new(GraphSearchTool::new(graph.clone())));
            registry.register(Box::new(GraphContextTool::new(graph.clone())));
            registry.register(Box::new(GraphImpactTool::new(graph.clone())));
            registry.register(Box::new(GraphPathTool::new(graph.clone())));
        }
    }

    /// Register the spawn-agent tool once the IPC layer has wired in a
    /// spawner. Without one there's no way to start a background agent, so the
    /// tool is omitted entirely (rather than erroring at call time). When this
    /// agent has a runtime id, the tool is made parent-aware so background
    /// agents it spawns are registered as its children (closing the
    /// completion-notification feedback loop).
    fn register_spawn_tool(
        &self,
        registry: &mut ToolRegistry,
        agent_id: Option<crate::runtime::AgentId>,
    ) {
        if let Some(spawner) = self.spawner.read().expect("spawner lock poisoned").clone() {
            let mut tool = match agent_id {
                Some(id) => SpawnAgentTool::new(spawner).with_parent(id),
                None => SpawnAgentTool::new(spawner),
            };
            // Give the tool access to the model resolver so its optional
            // `model` parameter can resolve a bare model id to an endpoint +
            // model. When no resolver is wired, the tool still works — it just
            // rejects a `model` arg with "model selection unavailable".
            if let Some(resolver) = &self.model_resolver {
                tool = tool.with_model_resolver(Arc::clone(resolver));
            }
            // Seed the spawned agent's first prompt with passively recalled
            // memory (the RECALLED CONTEXT rider) when a store is wired.
            if let Some(memory) = &self.memory {
                tool = tool.with_memory(Arc::clone(memory));
            }
            registry.register(Box::new(tool));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SafetyMode;
    use crate::memory::embedder::HashEmbedder;
    use crate::memory::MemoryStore;
    use crate::provider::{
        Capabilities, FinishReason, LlmEvent, Message, ProviderKind, ToolSchema,
    };
    use crate::tool::workflow::plan::CreatePlanTool;
    use crate::tool::Tool;
    use crate::workflow::WorkflowState;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::tempdir;

    /// A mock provider that returns a canned sequence of responses.
    struct MockProvider {
        responses: Arc<tokio::sync::Mutex<std::collections::VecDeque<Vec<LlmEvent>>>>,
        caps: Capabilities,
    }

    impl MockProvider {
        fn sequence(responses: Vec<Vec<LlmEvent>>) -> Self {
            Self {
                responses: Arc::new(tokio::sync::Mutex::new(responses.into())),
                caps: Capabilities::openai(),
            }
        }
    }

    #[async_trait]
    impl LlmClient for MockProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            let mut responses = self.responses.lock().await;
            let events = if let Some(front) = responses.pop_front() {
                front
            } else {
                vec![LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }]
            };
            drop(responses);
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// Build a factory over a temp dir with a mock provider + in-memory memory.
    fn make_factory(dir: &std::path::Path) -> AgentLoopFactory {
        make_factory_with(dir, None)
    }

    /// Build a factory with an optional vision model (to exercise the
    /// `describe_image` tool registration).
    fn make_factory_with(
        dir: &std::path::Path,
        vision: Option<Arc<dyn ImageDescriber>>,
    ) -> AgentLoopFactory {
        let provider: Arc<dyn LlmClient> = Arc::new(MockProvider::sequence(vec![]));
        let sandbox = Arc::new(Sandbox::new(dir).unwrap());
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        let gpath = dir.join("global.md");
        let ppath = dir.join("project.md");
        std::fs::write(&gpath, "").unwrap();
        std::fs::write(&ppath, "").unwrap();
        let source = ConstitutionSource::new(&gpath, &ppath).unwrap();
        AgentLoopFactory::new(
            provider,
            source,
            Some(store),
            sandbox,
            dir.to_path_buf(),
            None,
            Arc::new(std::sync::RwLock::new(SafetyMode::Autonomous)),
            ContextManager::new(128_000, 0.5),
            dir.join("plans"),
            vision,
        )
    }

    /// A mock vision model for registration tests — never makes a network call.
    struct MockDescriber;

    #[async_trait]
    impl ImageDescriber for MockDescriber {
        async fn describe_image(
            &self,
            _image_url: &str,
            _prompt: &str,
        ) -> crate::error::Result<String> {
            Ok("mock description".into())
        }
    }

    #[tokio::test]
    async fn two_builds_produce_independent_workflows() {
        // The core multi-agent safety property: two agents built from the same
        // factory must have independent workflows. Creating a plan in agent A
        // must NOT affect agent B's state.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());

        let agent_a = factory.build();
        let agent_b = factory.build();

        // Both start in Planning (no plan on disk).
        {
            let wf_a_arc = agent_a.workflow_handle();
            let wf_b_arc = agent_b.workflow_handle();
            let wf_a = wf_a_arc.lock().await;
            let wf_b = wf_b_arc.lock().await;
            assert_eq!(wf_a.state(), WorkflowState::Planning);
            assert_eq!(wf_b.state(), WorkflowState::Planning);
        }

        // Create a plan in agent A's workflow directly.
        {
            let wf_a_arc = agent_a.workflow_handle();
            let mut wf_a = wf_a_arc.lock().await;
            wf_a.create_plan("A's plan", "goal", "ctx", vec!["step1".into()])
                .unwrap();
        }

        // Agent A is now Executing; agent B must still be Planning.
        {
            let wf_a_arc = agent_a.workflow_handle();
            let wf_b_arc = agent_b.workflow_handle();
            let wf_a = wf_a_arc.lock().await;
            let wf_b = wf_b_arc.lock().await;
            assert_eq!(wf_a.state(), WorkflowState::Executing);
            assert_eq!(
                wf_b.state(),
                WorkflowState::Planning,
                "agent B's workflow must be independent — not affected by A's plan"
            );
            assert!(wf_a.plan().is_some());
            assert!(wf_b.plan().is_none());
        }
    }

    #[tokio::test]
    async fn build_loads_existing_plan_from_disk() {
        // A freshly-built agent should resume an existing plan from disk
        // (load_latest), so spawning a second agent picks up the in-flight plan.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());

        // Agent A creates a plan (writes it to disk).
        let agent_a = factory.build();
        {
            let wf_a_arc = agent_a.workflow_handle();
            let mut wf_a = wf_a_arc.lock().await;
            wf_a.create_plan("Resume me", "goal", "ctx", vec!["a".into(), "b".into()])
                .unwrap();
            wf_a.complete_step(0).unwrap();
        }

        // Agent B is built from the same factory — it should load the plan
        // from disk and resume at step 1 (Executing, one step done).
        let agent_b = factory.build();
        let wf_b_arc = agent_b.workflow_handle();
        let wf_b = wf_b_arc.lock().await;
        assert_eq!(wf_b.state(), WorkflowState::Executing);
        assert!(wf_b.plan().is_some());
        assert_eq!(wf_b.plan().unwrap().completed_count(), 1);
        assert_eq!(wf_b.current_step().unwrap().text, "b");
    }

    #[tokio::test]
    async fn factory_shares_safety_mode_across_builds() {
        // The safety-mode handle is shared across all builds from a factory,
        // so a runtime toggle affects every agent. Verify by toggling after
        // building two agents and checking both see the new mode.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());

        let agent_a = factory.build();
        let agent_b = factory.build();

        // Both start in Autonomous (the factory's initial mode).
        assert_eq!(
            *agent_a.safety_mode_handle().read().unwrap(),
            SafetyMode::Autonomous
        );
        assert_eq!(
            *agent_b.safety_mode_handle().read().unwrap(),
            SafetyMode::Autonomous
        );

        // Toggle via agent A's handle — agent B must see the change too.
        *agent_a.safety_mode_handle().write().unwrap() = SafetyMode::ApproveEachAction;
        assert_eq!(
            *agent_b.safety_mode_handle().read().unwrap(),
            SafetyMode::ApproveEachAction,
            "safety mode is shared — toggling via A must affect B"
        );
    }

    #[tokio::test]
    async fn factory_set_core_operations_observed_by_built_git_tool() {
        // The core-operations handle is shared across all builds from a factory,
        // so a `save_settings` call (which calls `factory.set_core_operations`)
        // is observed by an already-built GitTool on its next `never_auto_for`
        // check — no registry rebuild needed. This is the factory-level
        // integration test for the live-rewire path (the unit test in git.rs
        // mutates the shared Arc directly; this one exercises the factory's
        // public setter against a real built agent's git tool).
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());

        let agent = factory.build();
        let git = agent.tools().get("git").expect("git tool registered");

        // Default list = merge/push → checkout is NOT a core op.
        assert!(!git.never_auto_for(&json!({"subcommand": "checkout"})));
        assert!(git.never_auto_for(&json!({"subcommand": "merge"})));

        // Mirror save_settings: push a new list via the factory's public
        // setter. The already-built agent's GitTool shares the handle, so it
        // observes the change on the next never_auto_for check.
        factory.set_core_operations(vec![
            "merge".to_string(),
            "push".to_string(),
            "checkout".to_string(),
        ]);
        assert!(
            git.never_auto_for(&json!({"subcommand": "checkout"})),
            "checkout must be gated after set_core_operations adds it"
        );
        // Follow-up A (2027-01-11): the SHELL tool shares that same handle, so
        // `shell git push`/`git merge` raise the always-on approval prompt too —
        // and it follows the runtime list rather than a hard-coded default.
        let shell = agent.tools().get("shell").expect("shell tool registered");
        assert!(shell.never_auto_for(&json!({"command": "git push origin main"})));
        assert!(shell.never_auto_for(&json!({"command": "cd repo && git merge --no-ff wt/x"})));
        assert!(
            shell.never_auto_for(&json!({"command": "git checkout main"})),
            "the shell guard must observe set_core_operations, not a default list"
        );
        assert!(!shell.never_auto_for(&json!({"command": "git status"})));
        assert!(!shell.never_auto_for(&json!({"command": "echo git-push-note"})));
        assert!(git.never_auto_for(&json!({"subcommand": "merge"})));
        assert!(!git.never_auto_for(&json!({"subcommand": "status"})));
    }

    #[tokio::test]
    async fn factory_set_shell_filter_config_observed_by_built_shell_tool() {
        // The shell-filter config handle is shared across all builds from a
        // factory, so a `save_settings` call (which calls
        // `factory.set_shell_filter_config`) is observed by an already-built
        // ShellTool on its next command — no registry rebuild needed.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let agent = factory.build();
        let shell = agent.tools().get("shell").expect("shell tool registered");
        let cmd = if cfg!(target_os = "windows") {
            "1..4 | ForEach-Object { 'same' }"
        } else {
            "for i in 1 2 3 4; do echo same; done"
        };

        // Default config (enabled) → the repeated line is deduped.
        let result = shell.execute(json!({"command": cmd, "purpose": "filter dedup"})).await;
        assert!(result.success);
        assert_eq!(
            result.output.matches("same").count(),
            1,
            "default filter should dedup, got: {}",
            result.output
        );

        // Mirror save_settings: disable the filter via the factory's public
        // setter. The already-built agent's ShellTool shares the handle, so
        // it observes the change on the next command.
        factory.set_shell_filter_config(ShellFilterConfig {
            enabled: false,
            overrides: vec![],
        });
        let result = shell.execute(json!({"command": cmd, "purpose": "filter passthrough"})).await;
        assert!(result.success);
        assert_eq!(
            result.output.matches("same").count(),
            4,
            "disabled filter must pass through, got: {}",
            result.output
        );
    }

    /// The serialized size of a filter's `tools` array, in characters.
    ///
    /// Approximates what the provider actually receives: name + description +
    /// the parameters JSON, which is what the request body carries.
    fn tools_array_chars(
        registry: &ToolRegistry,
        filter: &crate::tool::ToolFilter,
    ) -> (usize, usize) {
        let caps = crate::provider::Capabilities::openai();
        let schemas = registry.schemas(&caps, filter);
        let chars: usize = schemas
            .iter()
            .map(|s| {
                s.name.len()
                    + s.description.len()
                    + serde_json::to_string(&s.parameters)
                        .map(|j| j.len())
                        .unwrap_or(0)
            })
            .sum();
        (schemas.len(), chars)
    }

    /// The `tools` array is the single largest fixed cost in every request —
    /// several times the system prompt. This is a BUDGET guard, not a style
    /// check: it fails when a new tool (or a re-inflated description) pushes
    /// a workflow state over its ceiling, so the regression is caught at the
    /// commit that causes it rather than months later on a token bill.
    ///
    /// Raising a ceiling is a legitimate outcome — but it should be a
    /// deliberate edit to this test, with the new tool justified, not a
    /// silent drift.
    #[test]
    fn tools_array_stays_within_context_budget() {
        use crate::tool::ToolFilter;
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(&workflow, None, None, factory.plans_dir());

        // Default config: no vision model, browser inspection off — the
        // `image_*` and on-screen `browser_*` tools hide themselves.
        assert!(!factory.vision_slot().is_configured());
        assert!(!factory.browser_inspection_enabled());

        // Apples-to-apples with the pre-disclosure behaviour only exists with
        // the browser feature on (the browser group carries ~900 tokens of
        // deferred schema); light builds skip this block.
        //
        // Windows-only too (user decision 2026-09-20): the browser group
        // is platform-conditional — the six on-screen WebView2 tools
        // (register_browser_tools, cfg(windows)) exist only on Windows, so
        // on macOS the all-loaded array is ~3.5k chars smaller and the
        // deferral delta falls below the 12k threshold (measured 14700 on
        // Windows vs 11246 on the macOS CI leg, run 35521221353 — the
        // default array is identical, 32796 chars, on both sides). The
        // assertion is calibrated to the Windows browser group; macOS
        // skips it (the ceilings loop below still runs there).
        #[cfg(all(feature = "browser", windows))]
        {
            // a fully
            // configured install (vision on, CDP on) with every group already
            // revealed is exactly what the old code shipped on every request.
            let f2 = make_factory_with(dir.path(), Some(Arc::new(MockDescriber)));
            f2.set_browser_inspection(true);
            let wf2 = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans2"))));
            let r2 = f2.build_registry(&wf2, None, None, f2.plans_dir());
            let (n0, c0) = tools_array_chars(&r2, &ToolFilter::Executing);
            r2.load_group("browser");
            r2.load_group("image");
            let (n1, c1) = tools_array_chars(&r2, &ToolFilter::Executing);
            println!(
                "all groups loaded: {n1} tools, {c1} chars (~{} tok)",
                c1 / 4
            );
            println!("deferred default: {n0} tools, {c0} chars (~{} tok)", c0 / 4);
            // The whole point of progressive disclosure: on a fully
            // configured install the default array must be dramatically
            // smaller than the everything-loaded one. If a future change
            // un-defers a family (or adds a big always-on tool), this is the
            // assertion that notices.
            assert!(
                c1 >= c0 + 12_000,
                "deferral must save >12k chars on a fully configured install                  ({c1} loaded vs {c0} default) — did a group stop being deferred?"
            );
        }

        // Ceilings in characters (~4 chars/token). Set from the measured
        // sizes with modest headroom.
        //
        // 2026-09-08: Executing raised 23_000 → 23_500 and ExecutingResearch
        // 20_000 → 20_100 ON PURPOSE for the git tool's structured `restore`
        // subcommand (paths/source/target fields + enum entry, ~700 chars) —
        // the whole point of that change is that restore takes structured
        // fields instead of free-form args, so the schema text is the API
        // surface and can't be trimmed away.
        // 2026-09-19: ExecutingResearch raised 20_100 → 20_300 for the git
        // tool's `branch list` read-only args (allowlist) — `branch list` is a
        // read query that now accepts listing flags like -vv/-a/-r, so the
        // `args` schema description grew to name it. Schema text is the API
        // surface; the description is trimmed to the essentials.
        // 2026-12-04: three ceilings raised ON PURPOSE for the
        // finish↔update_plan deadlock fix: Executing 23_500 → 23_600 and
        // ExecutingResearch 20_300 → 20_400 (finish gained an optional
        // `regression_test` property, ~37 chars), and Reviewing 18_500 →
        // 20_700 (update_plan is now visible in Reviewing — its full schema
        // joins the Reviewing array; since 2026-12-23 the workflow method
        // accepts title/goal/context/steps there too, matching Executing
        // minus complete_step).
        for (filter, ceiling) in [
            // 15_000 → 15_300 (2026-09-08): the first full `cargo test
            // --workspace` matrix measures Planning at 15_241 chars. The
            // workspace run unifies features with src-tauri (browser on),
            // which registers `load_tools` in every filter — the deferral
            // loader only exists when a deferred group (the browser
            // family) does — adding +431 chars over the standalone
            // `cargo test -p mnemo --lib` measurement (14_810, no
            // load_tools — under the old ceiling, so this filter was
            // never over until the workspace matrix ran). Ceiling =
            // measured + headroom, deliberate raise.
            // 15_300 → 16_000 (2026-09-10): memory_amend (plan be16ea36) —
            // the sanctioned knowledge-amendment tool — joins the Planning
            // set (registered whenever the knowledge backing is wired, the
            // default); measures Planning at 15_751 chars. Ceiling =
            // measured + headroom, deliberate raise.
            // 16_000 → 16_700 (2027-01-10): measurement pass measures
            // Planning at 16_377 chars — +626 of schema growth since the
            // 15_751 baseline (2026-09-10), from the plan resumability-gate
            // commit 077d375 (create_plan's schema documents the gate;
            // create_plan rides Planning). The growth crosses the
            // workspace-scale ceiling while the standalone scale still
            // passes (~15_946 < 16_000), which is why plain `cargo test`
            // stayed green while `cargo test --workspace` failed. Ceiling =
            // measured + headroom, deliberate raise.
            // 16_700 → 18_200 (2027-01-15): measures Planning at 17_871
            // chars (workspace-unified; standalone 17_387) — +1_494 over the
            // 16_377 workspace baseline recorded at the 2027-01-10 pass. The
            // growth is this change's memory_update schema (the
            // find/replace_with targeted-repair mode) + memory_amend's
            // heading-strip documentation (backlog 488248ce) plus schema
            // drift from plans landed since that pass. Ceiling = measured +
            // headroom, deliberate raise.
            // 18_200 → 18_700 (2027-02-05): the search tool's description
            // gains the ESCAPE-HATCH note (backlog 38040f12 — literal:true
            // advertised as the remedy for metacharacter/backslash-dense
            // patterns, plus the malformed-rejection reformulation rule,
            // ~+247); measures Planning at 18_275 chars (workspace-unified;
            // standalone 17_938). Ceiling = measured + headroom, deliberate
            // raise.
            // 18_700 holds (2027-02-05, round 2): the search/search_read
            // ESCAPE-HATCH parity (~+330, same cause as the Executing raise)
            // rides Planning too — measures 18_605, 95 under; no raise
            // needed.
            // 18_700 → 18_900 (2027-02-05): git_read gains op="status"
            // (backlog 1aa7e456 — the description documents the new op and
            // the enum grows, ~+122); measures Planning at 18_727 chars.
            // Ceiling = measured + headroom, deliberate raise.
            // 18_900 → 19_500 (2027-02-05): the empty-call sweep (backlog
            // d9ad618e) — five of the six fumbled tools ride Planning
            // (graph_search/graph_context/graph_path/git_read/memory_write;
            // shell is Executing-side only), each description gaining the
            // no-zero-argument rule + example + recovery rule (~+900 chars
            // of description text). Workspace-unified measures Planning at
            // 19_241 chars on the final tree (standalone 18_757 + ~484
            // load_tools delta under browser-on unification — the
            // standalone run alone stays green; the workspace matrix is
            // what CI checks). Baseline note: the prior 18_727 figure's
            // comment stack was re-chained after the fact (the
            // git_read-status measurement stacked above the escape-hatch
            // figures though 37540e0 landed before 6f363f8), so 19_241 −
            // 18_727 = +514 understates the sweep — the pinned 19_241
            // printout is the authoritative reference for future raises.
            // Ceiling = measured + headroom, deliberate raise.
            (ToolFilter::Planning, 19_500),
            // 23_600 → 24_200 (2026-12-08): measured with the `browser`
            // feature enabled — Executing carries the browser tool family
            // (offscreen_browser_* + browser_*, incl. the file:// navigation
            // descriptions); the 2026-12-04 raise predates the unified
            // Browser-tab tool set, so the feature-gated array sat ~350 chars
            // over. Deliberate raise, not drift (see doc comment above).
            // 24_200 → 24_500 (2026-12-22): first `cargo test --workspace`
            // run (features unify with src-tauri — the production feature
            // set, which always selects `browser`) measures Executing at
            // 24_420 chars; the 2026-12-08 budget no longer fits under
            // unification. No schemas changed in plan ffe59699 — the growth
            // predates it. Ceiling = measured + headroom, deliberate raise.
            // 24_500 → 24_800 (2026-12-24): workspace feature unification
            // measures Executing at 24_507 chars; raise to headroom.
            // 24_800 → 25_000 (2027-01-07): the bug_fixing detailed-steps
            // persistence (plan 4405d82d / backlog 77ff8f45) documents the
            // crash-resumption contract in the create_plan description +
            // steps schema; measures Executing at 24_957 chars. The schema
            // text is the API surface the agent reads to know steps are
            // never discarded. Ceiling = measured + headroom, deliberate
            // raise (the headroom is thin — 43 chars — on purpose: the
            // measurement is exact and recorded here as the baseline).
            // 25_000 → 25_450 (2026-09-08): first full `cargo test
            // --workspace` matrix since the 2027-01-07 raise measures
            // Executing at 25_388 chars — the workspace run unifies
            // features with src-tauri (browser on), which registers
            // `load_tools` in every filter (the deferral loader only
            // exists when a deferred group — the browser family — does),
            // +431 chars over the standalone measurement (24_957 — the
            // exact figure the 2027-01-07 raise recorded, so that baseline
            // was taken without unification; no schema growth since).
            // Ceiling = measured + headroom, deliberate raise.
            // 25_450 → 28_000 (2026-09-10): memory_amend (plan be16ea36)
            // joins the set (+510) and file_edit's schema grows for the
            // batch (`edits`) + append modes (+~1,800 — the two new
            // properties and the six-mode description); measures Executing
            // at 27_696 chars. Ceiling = measured + headroom, deliberate
            // raise.
            // 28_000 → 28_800 (2027-01-10): measurement pass measures
            // Executing at 28_453 chars — +757 of schema growth since the
            // 27_696 baseline (2026-09-10), from the plan resumability-gate
            // commit 077d375 (create_plan + update_plan schema docs; both
            // ride Executing). Ceiling = measured + headroom, deliberate
            // raise.
            // 28_800 → 29_500 (2027-01-11): the checkable detailed
            // sub-steps (plan 9441d776 — complete_step gains the
            // detailed_step_index property + its description sentence, and
            // step_index's description notes the exactly-one rule) grow
            // the schema ~+584; measures Executing at 29_037 chars
            // (workspace-unified). Ceiling = measured + headroom,
            // deliberate raise.
            // 29_500 → 31_150 (2027-01-15): measures Executing at 30_813
            // chars (workspace-unified; standalone 30_329) — +1_776 over the
            // 29_037 workspace baseline recorded at the 2027-01-10 pass. Same
            // cause as Planning above (backlog 488248ce's memory_update/amend
            // schema growth, plus drift since that pass). Ceiling = measured +
            // headroom, deliberate raise.
            // 31_150 → 32_100 (2027-01-24): strict-schema normalization
            // (plan 21118961) rewrites the STRICT_TOOLS schemas to
            // strict-legal form on strict-capable endpoints — optional
            // properties widen to nullable type-arrays, every object
            // gains additionalProperties/required keys; measures Executing
            // at 31_471 chars. Ceiling = measured + headroom, deliberate
            // raise.
            // 32_100 → 32_300 (2027-01-24): backlog 9118714a (plan
            // 50f36b1e) — the null-stringify defense round: optional
            // string/enum params widen to ["string","null"] across the
            // victim tools (~+100) and file_edit's new_string description
            // gains the literal-'null' trap note (~+150); measures
            // Executing at 32_118 chars. Ceiling = measured + headroom,
            // deliberate raise.
            // 32_300 → 32_500 (2027-01-24): backlog 37f8631a (plan
            // 5f6e593d) — update_plan's schema description and append
            // param description gain the Reviewing append-window wording
            // (~+194); measures Executing at 32_312 chars. Ceiling =
            // measured + headroom, deliberate raise.
            // 32_500 → 33_200 (2027-02-05): the offscreen browser timeout
            // descriptions (plan ec425270 — offscreen_browser_navigate +
            // offscreen_browser_eval document the ~60s/30s abort +
            // automatic-restart behavior, ~+484); measures Executing at
            // 32_796 chars. Ceiling = measured + headroom, deliberate
            // raise.
            // 33_200 → 33_900 (2027-02-05): the search/search_read
            // ESCAPE-HATCH parity (backlog 38040f12 review round 1 —
            // search_read gains the note + RECOMMENDED wording, search
            // gains the inline literal example, ~+330); measures Executing
            // at 33_385 chars. Ceiling = measured + headroom, deliberate
            // raise.
            // 33_900 → 34_600 (2027-02-05): the empty-call sweep (backlog
            // d9ad618e) — all six fumbled tools ride Executing (shell +
            // the graph trio + git_read + memory_write), each description
            // gaining the no-zero-argument rule + example + recovery rule
            // (~+1_050 chars of description text); workspace-unified
            // measures Executing at 34_169 chars on the final tree
            // (standalone 33_685 + ~484 load_tools delta; the pre-sweep
            // baselines in this stack were re-chained — see the Planning
            // note — so delta-vs-baseline arithmetic understates the
            // sweep; the pinned printouts are authoritative). Ceiling =
            // measured + headroom, deliberate raise.
            // 34_600 → 36_300 (2026-09-23): the multi_edit tool (backlog
            // 2e27f896 — atomic multi-file edits sharing file_edit's op
            // engine) joins the Agent set, and file_edit's batch field became
            // the polymorphic `ops` array (compact line ops + anchor objects,
            // one field carrying both forms) — the pair measures Executing at
            // 35_999 chars. multi_edit's schema keeps only a pointer to
            // file_edit's grammar (both ride the same tools array); the rest
            // is API surface. Ceiling = measured + headroom, deliberate
            // raise.
            (ToolFilter::Executing, 36_300),
            // PlanFrozen joins the budget guard with this change (2027-01-10):
            // it is the production surface for every implementation/bug_fixing
            // plan — the largest array the app sends (Executing ∪ finish) —
            // so it needs its own ceiling, not coverage-by-transitivity.
            // Measured at 29_565 chars (25 tools) on the 2027-01-10 pass.
            // Ceiling = measured + headroom, deliberate raise.
            // 29_900 → 30_600 (2027-01-11): the checkable detailed
            // sub-steps (plan 9441d776 — complete_step's
            // detailed_step_index property + description growth, ~+584)
            // ride PlanFrozen with the rest of Executing; measures
            // PlanFrozen at 30_149 chars (workspace-unified). Ceiling =
            // measured + headroom, deliberate raise.
            // 30_600 → 32_400 (2027-01-15): measures PlanFrozen at 32_043
            // chars (workspace-unified; standalone 31_559) — the same
            // 2027-01-15 pass and cause as Executing above (PlanFrozen =
            // Executing ∪ finish). Ceiling = measured + headroom, deliberate
            // raise.
            // 32_400 → 33_400 (2027-01-24): strict-schema normalization
            // (plan 21118961) — same cause and figure as the Executing
            // raise above (PlanFrozen = Executing ∪ finish, and finish is
            // a STRICT_TOOLS member); measures PlanFrozen at 32_757
            // chars. Ceiling = measured + headroom, deliberate raise.
            // 33_400 → 33_600 (2027-01-24): backlog 9118714a (plan
            // 50f36b1e) — same cause as the Executing raise above
            // (nullable victim schemas + file_edit's new_string trap
            // note); measures PlanFrozen at 33_404 chars. Ceiling =
            // measured + headroom, deliberate raise.
            // 33_600 → 33_800 (2027-01-24): backlog 37f8631a (plan
            // 5f6e593d) — same cause as the Executing raise above
            // (update_plan's append-window wording, ~+194; PlanFrozen =
            // Executing ∪ finish); measures PlanFrozen at 33_598 chars —
            // 2 under the old ceiling, raised for real headroom.
            // Ceiling = measured + headroom, deliberate raise.
            // 33_800 → 34_500 (2027-02-05): same cause as the Executing
            // raise above (the offscreen browser timeout descriptions,
            // plan ec425270, ~+484; PlanFrozen = Executing ∪ finish);
            // measures PlanFrozen at 34_082 chars. Ceiling = measured +
            // headroom, deliberate raise.
            // 34_500 → 35_200 (2027-02-05): same cause as the Executing
            // raise above (the search/search_read ESCAPE-HATCH parity,
            // backlog 38040f12, ~+330; PlanFrozen = Executing ∪ finish);
            // measures PlanFrozen at 34_671 chars. Ceiling = measured +
            // headroom, deliberate raise.
            // 35_200 → 35_900 (2027-02-05): same cause as the Executing
            // raise (backlog d9ad618e, the six-description empty-call
            // sweep); workspace-unified measures PlanFrozen at 35_455
            // chars (standalone 34_971 + ~484 load_tools delta). Ceiling
            // = measured + headroom, deliberate raise.
            // 35_900 → 37_600 (2026-09-23): same cause as the Executing raise
            // above (multi_edit joins the Agent set + file_edit's polymorphic
            // `ops` field; PlanFrozen = Executing ∪ finish); measures
            // PlanFrozen at 37_285 chars. Ceiling = measured + headroom,
            // deliberate raise.
            (ToolFilter::PlanFrozen, 37_600),
            // 20_400 → 21_100 (2026-12-08): same browser-feature measurement
            // as Executing above — research filters carry the browser tools.
            // 21_100 → 21_600 (2027-01-07): same change (plan 4405d82d /
            // backlog 77ff8f45) — create_plan rides the research filters
            // too; measures ExecutingResearch at 21_543 chars. Same
            // deliberate raise as Executing above (57 chars of headroom,
            // measured baseline recorded); ceiling = measured + headroom.
            // 21_600 → 22_050 (2026-09-08): same workspace-unification
            // measurement pass as Executing above (load_tools, +431);
            // measures ExecutingResearch at 21_974 chars (standalone:
            // 21_543 — the 2027-01-07 figure, no schema growth since).
            // Ceiling = measured + headroom, deliberate raise.
            // 22_050 → 22_800 (2026-09-10): memory_amend (plan be16ea36)
            // joins the set (+510 — the research filters exclude the
            // file-mutation trio, so no file_edit schema growth rides
            // here); measures ExecutingResearch at 22_484 chars. Ceiling =
            // measured + headroom, deliberate raise.
            // 22_800 → 23_600 (2027-01-10): measurement pass measures
            // ExecutingResearch at 23_242 chars — +758 of schema growth
            // since the 22_484 baseline (2026-09-10), from the plan
            // resumability-gate commit 077d375 (create_plan's schema docs;
            // create_plan rides the research filters). Ceiling = measured +
            // headroom, deliberate raise.
            // 23_600 → 24_300 (2027-01-11): the checkable detailed
            // sub-steps (plan 9441d776 — complete_step's
            // detailed_step_index property + description growth, ~+584)
            // ride the research filters with create_plan; measures
            // ExecutingResearch at 23_826 chars (workspace-unified).
            // Ceiling = measured + headroom, deliberate raise.
            // 24_300 → 25_950 (2027-01-15): measures ExecutingResearch at
            // 25_605 chars (workspace-unified; standalone 25_121) — the same
            // 2027-01-15 pass and cause as Planning above (it carries
            // memory_update + memory_amend too). Ceiling = measured +
            // headroom, deliberate raise.
            // 25_950 → 26_800 (2027-01-24): the shell tool's description
            // gains the calling-trap notes (backlog 79a2755d — the
            // empty-argument trap and the PowerShell 5.1 && / ||
            // auto-translate advisory, ~+350); measures ExecutingResearch
            // at 26_243 chars. Ceiling = measured + headroom, deliberate
            // raise.
            // 26_800 → 27_300 (2027-02-05): same cause as the Executing
            // raise above (the offscreen browser timeout descriptions,
            // plan ec425270, ~+484 — the browser family rides the research
            // filters — plus ~+194 of update_plan append-window wording
            // that landed after this filter's 2027-01-24 baseline);
            // measures ExecutingResearch at 26_921 chars. Ceiling =
            // measured + headroom, deliberate raise.
            // 27_300 → 28_000 (2027-02-05): same cause as the Executing
            // raise above (the search/search_read ESCAPE-HATCH parity,
            // backlog 38040f12, ~+330); measures ExecutingResearch at
            // 27_510 chars. Ceiling = measured + headroom, deliberate
            // raise.
            // 28_000 → 28_600 (2027-02-05): same cause as the Executing
            // raise (backlog d9ad618e, the six-description empty-call
            // sweep); workspace-unified measures ExecutingResearch at
            // 28_294 chars (standalone 27_810 + ~484 load_tools delta).
            // Ceiling = measured + headroom, deliberate raise.
            (ToolFilter::ExecutingResearch, 28_600),
            // 20_700 → 21_300 (2026-12-08): Reviewing likewise carries the
            // browser tool family (the reviewer drives the visible Browser
            // tab), so the feature-gated array was ~410 over. Deliberate
            // raise, not drift.
            // 21_300 → 21_500 (2026-12-22): first `cargo test --workspace`
            // run (features unify with src-tauri — the production feature
            // set) measures Reviewing at 21_336 chars. Same unification
            // effect as Executing above; no schemas changed in plan ffe59699.
            // Deliberate raise to measured + headroom.
            // 21_500 → 21_950 (2026-09-08): the same workspace-unification
            // measurement pass measures Reviewing at 21_856 chars
            // (standalone: 21_425 — the +431 is load_tools, as above).
            // Growth since the last recorded workspace baseline (21_336,
            // 2026-12-22 — itself a unified measurement that already
            // included load_tools) is +520 of schema growth: the
            // 2026-12-23 update_plan Reviewing-surface expansion plus the
            // 2027-01-07 create_plan/steps-persistence changes riding
            // update_plan into Reviewing. Ceiling = measured + headroom,
            // deliberate raise.
            // 21_950 → 24_500 (2026-09-10): memory_amend (plan be16ea36)
            // joins the set (+510) and file_edit's schema grows for the
            // batch (`edits`) + append modes (+~1,800 — Reviewing carries
            // file_edit, unlike the research filters); measures Reviewing
            // at 24_164 chars. Ceiling = measured + headroom, deliberate
            // raise.
            // 24_500 → 25_200 (2027-01-10): measurement pass measures
            // Reviewing at 24_779 chars — +615 of schema growth since the
            // 24_164 baseline (2026-09-10), from the plan resumability-gate
            // commit 077d375 (update_plan's schema docs; update_plan rides
            // Reviewing while create_plan does not — matching the smaller
            // delta). Ceiling = measured + headroom, deliberate raise.
            // 25_200 → 26_600 (2027-01-15): measures Reviewing at 26_279
            // chars (workspace-unified; standalone 25_795) — the same
            // 2027-01-15 pass and cause as Planning (memory_update/amend ride
            // Reviewing too). Ceiling = measured + headroom, deliberate
            // raise.
            // 26_600 → 27_300 (2027-01-24): strict-schema normalization
            // (plan 21118961) — Reviewing carries several STRICT_TOOLS
            // members (file_edit, update_plan, memory_update/amend);
            // measures Reviewing at 26_735 chars. Ceiling = measured +
            // headroom, deliberate raise.
            // 27_300 → 27_500 (2027-01-24): backlog 9118714a (plan
            // 50f36b1e) — same cause as the Executing raise above
            // (nullable victim schemas + file_edit's new_string trap
            // note); measures Reviewing at 27_382 chars. Ceiling =
            // measured + headroom, deliberate raise.
            // 27_500 → 27_800 (2027-01-24): backlog 37f8631a (plan
            // 5f6e593d) — same cause as the Executing raise above
            // (update_plan's append-window wording rides Reviewing,
            // ~+194); measures Reviewing at 27_576 chars. Ceiling =
            // measured + headroom, deliberate raise.
            // 27_800 → 28_400 (2027-02-05): same cause as the Executing
            // raise above (the offscreen browser timeout descriptions,
            // plan ec425270, ~+484 — the browser family rides Reviewing);
            // measures Reviewing at 28_060 chars. Ceiling = measured +
            // headroom, deliberate raise.
            // 28_400 → 29_200 (2027-02-05): same cause as the Executing
            // raise above (the search/search_read ESCAPE-HATCH parity,
            // backlog 38040f12, ~+330); measures Reviewing at 28_649
            // chars. Ceiling = measured + headroom, deliberate raise.
            // 29_200 → 29_800 (2027-02-05): same cause as the Executing
            // raise (backlog d9ad618e, the six-description empty-call
            // sweep); workspace-unified measures Reviewing at 29_433
            // chars (standalone 28_949 + ~484 load_tools delta). Ceiling
            // = measured + headroom, deliberate raise.
            // 29_800 → 31_600 (2026-09-23): same cause as the Executing raise
            // above (multi_edit joins the set + file_edit's polymorphic `ops`
            // field) — the Reviewer's read-only agent still carries the Agent
            // file tools' SCHEMAS (it cannot call the mutation tools, but the
            // advertised array is shared); measures Reviewing at 31_263
            // chars. Ceiling = measured + headroom, deliberate raise.
            (ToolFilter::Reviewing, 31_600),
            // 15_000 → 15_300 (2026-09-08): same workspace-unification
            // measurement pass as Executing above (load_tools, +431);
            // measures Complete at 15_241 chars (standalone: 14_810 —
            // the same tool set as Planning, which shared this 15_000
            // ceiling). Ceiling = measured + headroom, deliberate raise.
            // 15_300 → 16_000 (2026-09-10): memory_amend (plan be16ea36)
            // joins the set — same tool set as Planning, same +510 growth
            // (measures 15_751). Ceiling = measured + headroom, deliberate
            // raise.
            // 16_000 → 16_700 (2027-01-10): measurement pass measures
            // Complete at 16_377 chars — the same tool set as Planning, the
            // same +626 growth from the plan resumability-gate commit
            // 077d375 (create_plan's schema docs; create_plan rides
            // Complete). Ceiling = measured + headroom, deliberate raise.
            // 16_700 → 18_200 (2027-01-15): measures Complete at 17_871
            // chars — the same tool set, figures and cause as Planning
            // (create_plan rides Complete). Ceiling = measured + headroom,
            // deliberate raise.
            // 18_200 → 18_700 (2027-02-05): same cause as the Planning raise
            // above (the search ESCAPE-HATCH note, backlog 38040f12, ~+247);
            // measures Complete at 18_275 chars. Ceiling = measured +
            // headroom, deliberate raise.
            // 18_700 holds (2027-02-05, round 2): same +330 parity growth as
            // Planning — measures Complete at 18_605, 95 under; no raise
            // needed.
            // 18_700 → 18_900 (2027-02-05): same cause as the Planning raise
            // above (git_read op="status", backlog 1aa7e456, ~+122);
            // measures Complete at 18_727 chars. Ceiling = measured +
            // headroom, deliberate raise.
            // 18_900 → 19_500 (2027-02-05): same cause as the Planning
            // raise (backlog d9ad618e — Complete carries the same read-only
            // set); workspace-unified measures Complete at 19_241 chars
            // (standalone 18_757 + ~484 load_tools delta). Ceiling =
            // measured + headroom, deliberate raise.
            (ToolFilter::Complete, 19_500),
        ] {
            let (n, chars) = tools_array_chars(&registry, &filter);
            println!(
                "{filter:?}: {n} tools, {chars} chars (~{} tokens)",
                chars / 4
            );
            assert!(
                chars <= ceiling,
                "{filter:?} tools array is {chars} chars (~{} tokens), over the {ceiling} budget \
                 — add the tool behind a gate, trim a description, or raise this ceiling on purpose",
                chars / 4
            );
        }
    }

    #[cfg(all(windows, feature = "browser"))]
    #[test]
    fn onscreen_browser_tools_advertised_only_with_inspection_enabled() {
        // The six on-screen browser_* tools attach to the app's WebView2 over
        // its CDP endpoint, which exists only when the user opted in. With
        // `enable_browser_inspection = false` (the default) they are hidden
        // rather than advertised as a guaranteed failure. The headless
        // offscreen_browser_* family is unaffected.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(&workflow, None, None, factory.plans_dir());
        let caps = crate::provider::Capabilities::openai();

        assert!(
            !factory.browser_inspection_enabled(),
            "inspection is off by default"
        );
        let names = |r: &ToolRegistry| -> Vec<String> {
            r.schemas(&caps, &crate::tool::ToolFilter::Executing)
                .into_iter()
                .map(|s| s.name)
                .collect()
        };
        // TWO independent gates now stand between a browser tool and the
        // tools array: `available()` (is the CDP endpoint even there?) and
        // `deferred_group()` (has the agent asked for the browser group?).
        let off = names(&registry);
        assert!(
            registry.get("browser_eval").is_some(),
            "still REGISTERED — only advertising is gated"
        );
        assert!(
            !off.iter().any(|n| n.contains("browser_")),
            "the whole browser family is deferred until load_tools, got: {off:?}"
        );

        // Reveal the group: the headless family appears, but the on-screen
        // tools stay hidden because their CDP endpoint is still off.
        registry.load_group("browser");
        let loaded = names(&registry);
        assert!(
            loaded.iter().any(|n| n.starts_with("offscreen_browser_")),
            "the headless family appears once the group is loaded"
        );
        assert!(
            !loaded
                .iter()
                .any(|n| n.starts_with("browser_") && !n.starts_with("offscreen_")),
            "on-screen tools stay hidden while inspection is off, got: {loaded:?}"
        );

        // Flipping the shared flag takes effect on the next schemas() call —
        // no registry rebuild, which is what makes the Settings toggle live.
        factory.set_browser_inspection(true);
        let on = names(&registry);
        assert_eq!(
            on.iter()
                .filter(|n| n.starts_with("browser_") && !n.starts_with("offscreen_"))
                .count(),
            6,
            "all six on-screen tools return once inspection is enabled, got: {on:?}"
        );
    }

    #[cfg(feature = "browser")]
    #[test]
    fn browser_group_is_deferred_until_loaded() {
        // The browser family is ~900 tokens of schema a typical coding turn
        // never calls. It costs one index line until the agent asks for it.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(&workflow, None, None, factory.plans_dir());

        let (_, before) = tools_array_chars(&registry, &crate::tool::ToolFilter::Executing);
        assert!(
            registry
                .hidden_groups_for(&crate::tool::ToolFilter::Executing)
                .iter()
                .any(|(g, _)| *g == "browser"),
            "browser is advertised in the index while hidden"
        );

        let revealed = registry.load_group("browser");
        assert!(
            revealed.iter().any(|n| n == "offscreen_browser_navigate"),
            "load_group reports what it revealed, got: {revealed:?}"
        );
        assert!(
            registry
                .hidden_groups_for(&crate::tool::ToolFilter::Executing)
                .is_empty(),
            "a loaded group drops out of the index"
        );

        let (_, after) = tools_array_chars(&registry, &crate::tool::ToolFilter::Executing);
        assert!(
            after > before + 3_000,
            "revealing the browser group adds real schema weight ({before} -> {after})"
        );

        // Idempotent — a second load reveals nothing new.
        assert!(registry.load_group("browser").is_empty());
        // Unknown groups are inert rather than a panic.
        assert!(registry.load_group("nope").is_empty());
    }

    #[test]
    fn image_tools_always_registered_against_swappable_slot() {
        // Always registered so Settings can enable vision without rebuild.
        let dir = tempdir().unwrap();
        let factory = make_factory_with(dir.path(), Some(Arc::new(MockDescriber)));
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(&workflow, None, None, factory.plans_dir());
        // All 7 image_* tools are registered when a vision client is configured.
        for name in [
            "image_analysis",
            "image_analyze_chart",
            "image_diagnose_error",
            "image_extract_text",
            "image_ui_diff",
            "image_ui_to_artifact",
            "image_understand_diagram",
        ] {
            assert!(
                registry.get(name).is_some(),
                "{name} should be registered when a vision client is configured"
            );
        }
        assert!(factory.vision_slot().is_configured());

        // Empty slot still registers the tools (execute errors until set).
        let dir2 = tempdir().unwrap();
        let factory_no_vision = make_factory(dir2.path());
        let workflow2 = Arc::new(Mutex::new(Workflow::new(dir2.path().join("plans"))));
        let registry_no_vision = factory_no_vision.build_registry(&workflow2, None, None, factory_no_vision.plans_dir());
        assert!(
            registry_no_vision.get("image_analysis").is_some(),
            "image_* tools stay registered against the empty swappable slot"
        );
        assert!(!factory_no_vision.vision_slot().is_configured());

        // ...but they are NOT ADVERTISED while the slot is empty: an
        // unconfigured image tool can only return "not configured", and the
        // seven schemas cost ~640 tokens per request to say so. Registration
        // stays unconditional (so a Settings toggle needs no rebuild);
        // visibility is decided per turn by `Tool::available`.
        let caps = crate::provider::Capabilities::openai();
        let names: Vec<String> = registry_no_vision
            .schemas(&caps, &crate::tool::ToolFilter::Executing)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(
            !names.iter().any(|n| n.starts_with("image_")),
            "no image_* tool is advertised while vision is unconfigured, got: {names:?}"
        );
        // With vision configured the tools are AVAILABLE, but they are also
        // a deferred group: occasional work that a normal coding turn never
        // touches, so they cost one index line until asked for. Both gates
        // must open before a schema is spent.
        let advertised: Vec<String> = registry
            .schemas(&caps, &crate::tool::ToolFilter::Executing)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(
            !advertised.iter().any(|n| n.starts_with("image_")),
            "available but still deferred until load_tools, got: {advertised:?}"
        );
        assert!(
            registry
                .hidden_groups_for(&crate::tool::ToolFilter::Executing)
                .iter()
                .any(|(g, _)| *g == "image"),
            "the image group is offered in the index once vision is configured"
        );
        registry.load_group("image");
        let loaded: Vec<String> = registry
            .schemas(&caps, &crate::tool::ToolFilter::Executing)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            loaded.iter().filter(|n| n.starts_with("image_")).count(),
            7,
            "all 7 image_* tools arrive once the group is loaded"
        );

        // ...and an unconfigured slot keeps the group out of the index
        // entirely, so the model is never offered a door onto nothing.
        assert!(
            registry_no_vision
                .hidden_groups_for(&crate::tool::ToolFilter::Executing)
                .iter()
                .all(|(g, _)| *g != "image"),
            "no image group offered without a vision model"
        );

        // Runtime rewire enables the slot.
        factory_no_vision.set_vision(Some(Arc::new(MockDescriber)));
        assert!(factory_no_vision.vision_slot().is_configured());
    }

    /// A mock descendant tracker that always reports "no descendants running".
    struct MockDescendantTracker;

    #[async_trait]
    impl crate::runtime::DescendantTracker for MockDescendantTracker {
        async fn has_running_descendants(&self, _agent_id: u64) -> bool {
            false
        }
    }

    #[test]
    fn descendant_tracker_wired_into_built_loop() {
        // When the IPC layer wires a DescendantTracker via
        // set_descendant_tracker, every subsequently-built agent must carry it
        // so the dispatch layer can gate state transitions on running
        // subagents. A factory with no tracker set builds loops with None.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());

        // No tracker wired → built loop has no descendant_tracker.
        let agent_none = factory.build();
        assert!(
            agent_none.descendant_tracker.is_none(),
            "loop built before set_descendant_tracker must have no tracker"
        );

        // Wire the mock tracker → built loop carries it.
        factory.set_descendant_tracker(Arc::new(MockDescendantTracker));
        let agent_some = factory.build();
        assert!(
            agent_some.descendant_tracker.is_some(),
            "loop built after set_descendant_tracker must carry the tracker"
        );
    }

    /// A mock spawner for registration tests — never spawns anything.
    struct MockSpawner;

    #[async_trait]
    impl crate::runtime::AgentSpawner for MockSpawner {
        async fn spawn(
            &self,
            _name: &str,
            _task: &str,
            _role: Option<String>,
        ) -> Result<u64, String> {
            Ok(1)
        }
    }

    #[test]
    fn spawn_agent_tool_registered_only_when_spawner_wired() {
        // Without a spawner, the registry must NOT include spawn_agent (there's
        // no runtime to start a background agent, so the tool would only error).
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(&workflow, None, None, factory.plans_dir());
        assert!(
            registry.get("spawn_agent").is_none(),
            "spawn_agent should NOT be registered without a spawner"
        );

        // Once a spawner is wired in, every subsequent build gets the tool.
        factory.set_spawner(Arc::new(MockSpawner));
        let registry2 = factory.build_registry(&workflow, None, None, factory.plans_dir());
        assert!(
            registry2.get("spawn_agent").is_some(),
            "spawn_agent should be registered once a spawner is wired in"
        );
    }

    #[test]
    fn list_models_registered_only_when_resolver_wired() {
        // Without a model resolver, list_models is NOT registered (there's
        // nothing to list — no config access).
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(&workflow, None, None, factory.plans_dir());
        assert!(
            registry.get("list_models").is_none(),
            "list_models should NOT be registered without a model resolver"
        );

        // Once a resolver is wired in, every subsequent build gets the tool.
        let factory2 = make_factory(dir.path()).with_model_resolver(Arc::new(
            crate::model_resolver::ConfigModelResolver::new(
                Arc::new(std::sync::RwLock::new(crate::config::Config::default())),
                Arc::new(crate::provider::trace::LlmRequestLog::new()),
            ),
        ));
        let registry2 = factory2.build_registry(&workflow, None, None, factory2.plans_dir());
        assert!(
            registry2.get("list_models").is_some(),
            "list_models should be registered once a model resolver is wired in"
        );
    }

    #[test]
    fn auto_typing_gate_flips_live_through_the_factory() {
        // The gate's flag is the SAME Arc the rewire path flips via
        // `set_auto_typing_enabled`, so a Settings toggle reaches
        // already-built tools with no registry rebuild (backlog a147b63c).
        let dir = tempdir().unwrap();
        let handle = AutoTypingHandle {
            classifier: Arc::new(std::sync::RwLock::new(None)),
            enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let flag = Arc::clone(&handle.enabled);
        let factory = make_factory(dir.path()).with_auto_typing(handle);
        factory.set_auto_typing_enabled(true);
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
        factory.set_auto_typing_enabled(false);
        assert!(!flag.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn expected_tool_names_registered_when_fully_wired() {
        // Maint M5: a fully-wired factory (skills + memory + spawner) must
        // register the complete expected tool name set. This guards against a
        // registration being silently dropped when the grouped helpers are
        // edited.
        let dir = tempdir().unwrap();
        let mut factory = make_factory(dir.path());
        factory = factory.with_skills(Arc::new(SkillLibrary::from_registry(
            dir.path().join(".coding/skills"),
            crate::skill::SkillRegistry::new(),
        )));
        factory.set_spawner(Arc::new(MockSpawner));
        // Fully wired = backlog store too (the IPC layer always wires it in
        // the Ready path before building the main agent).
        factory.set_backlog(Arc::new(tokio::sync::Mutex::new(
            crate::backlog::BacklogStore::open(dir.path().join("backlog.jsonl")),
        )));
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(&workflow, None, None, factory.plans_dir());

        let expected = vec![
            // agent/file tools
            "read_files",
            "file_edit",
            "multi_edit",
            "file_write",
            "convert_line_endings",
            "shell",
            "search",
            "search_read",
            "git",
            // The reveal half of progressive disclosure (always registered;
            // its schema lists the groups from DEFERRED_GROUPS).
            "load_tools",
            // read-only git bridge tools (Phase 3)
            "git_read",
            // read-only URL fetch (always present; usable in any state for research)
            "web_fetch",
            // read-only diff + reviewer report channel (always registered)
            "write_review_report",
            // workflow/plan tools
            "create_plan",
            "complete_step",
            "update_plan",
            "abandon_plan",
            // finish (the Reviewing → Complete exit gate)
            "finish",
            // current_plan (read-only, always available)
            "current_plan",
            // ask_user (always available — asking isn't a mutation)
            "ask_user",
            // backlog_add (wired above via set_backlog)
            "backlog_add",
            // backlog_status (wired above via set_backlog)
            "backlog_status",
            // backlog_list (wired above via set_backlog — read-only query)
            "backlog_list",
            // skill tools
            "skill_start",
            "skill_end",
            "abandon_skill",
            // skill library tools (reload: every state; create: Executing only)
            "skill_reload",
            "skill_create",
            // vision (always) — the 7 image_* tools (replacing describe_image)
            "image_analysis",
            "image_analyze_chart",
            "image_diagnose_error",
            "image_extract_text",
            "image_ui_diff",
            "image_ui_to_artifact",
            "image_understand_diagram",
            // memory tools (write/recall/consolidate + the Phase-1 hygiene tools
            // + the Phase-3 retrieval tools)
            "memory_write",
            "memory_search",
            "memory_consolidate",
            "memory_update",
            "memory_supersede",
            "memory_delete",
            "memory_amend",
            // spawn (wired)
            "spawn_agent",
        ];
        // Headless-browser tools (only under the `browser` feature — they
        // share the factory's BrowserManager and the chromiumoxide seam).
        // Shadow the list (not plain extend) so the light build never needs
        // `mut` and stays warning-free under deny(warnings).
        #[cfg(feature = "browser")]
        let expected = {
            let mut v = expected;
            v.extend([
                "offscreen_browser_navigate",
                "offscreen_browser_list_pages",
                "offscreen_browser_close_page",
                "offscreen_browser_screenshot",
                "offscreen_browser_console",
                "offscreen_browser_snapshot",
                "offscreen_browser_eval",
                "offscreen_browser_click",
                "offscreen_browser_type",
                "offscreen_browser_switch_page",
            ]);
            // Windows-only: the live WebView2 (Browser tab) inspection + control
            // tools attach to the app's own WebView2 via CDP — registered only on
            // Windows targets (see register_browser_tools), so the expected set
            // is platform-conditional too.
            #[cfg(windows)]
            v.extend([
                "browser_screenshot",
                "browser_eval",
                "browser_snapshot",
                "browser_navigate",
                "browser_click",
                "browser_type",
            ]);
            v
        };
        for &name in &expected {
            assert!(
                registry.get(name).is_some(),
                "expected tool '{name}' to be registered"
            );
        }
        // The total count must match (no unexpected extras).
        assert_eq!(
            registry.iter().count(),
            expected.len(),
            "registered tool count should match the expected set exactly"
        );
    }

    #[test]
    fn the_factory_snapshot_captures_the_deferred_tools() {
        // The snapshot wiring is load-bearing: if a register_* call drifts
        // past the snapshot, the tool silently drops from the reveal
        // response's schema block (the empty-block failure mode —
        // byte-identical to the pre-feature response). The image family is
        // registered unconditionally (feature-independent) and deferred, so
        // it pins the capture in the default build; availability is the
        // renderer's concern (vision_ready() gates it at reveal time), not
        // the snapshot's.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(&workflow, None, None, factory.plans_dir());
        let snap = AgentLoopFactory::deferred_snapshot(&registry);
        let names: Vec<&str> = snap.iter().map(|t| t.name()).collect();
        for name in [
            "image_analysis",
            "image_analyze_chart",
            "image_diagnose_error",
            "image_extract_text",
            "image_understand_diagram",
            "image_ui_diff",
            "image_ui_to_artifact",
        ] {
            assert!(
                names.contains(&name),
                "the snapshot must capture {name}: {names:?}"
            );
        }
        assert!(
            snap.iter().all(|t| t.deferred_group().is_some()),
            "the snapshot carries only deferred tools"
        );
    }

    #[test]
    fn conditional_tools_absent_without_their_wiring() {
        // Maint M5: the skill, memory, and spawn tools must be ABSENT when
        // their wiring is missing (a bare make_factory has no skills, no
        // spawner — but it DOES have memory, so memory tools are present).
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(&workflow, None, None, factory.plans_dir());

        // No backlog store → no backlog_add.
        assert!(
            registry.get("backlog_add").is_none(),
            "'backlog_add' should NOT be registered without the store wired"
        );

        // No skills → no skill tools.
        for name in ["skill_start", "skill_end", "abandon_skill"] {
            assert!(
                registry.get(name).is_none(),
                "'{name}' should NOT be registered without a skill registry"
            );
        }
        // No spawner → no spawn_agent.
        assert!(
            registry.get("spawn_agent").is_none(),
            "spawn_agent should NOT be registered without a spawner"
        );
        // make_factory wires memory, so memory tools ARE present here.
        for name in [
            "memory_write",
            "memory_search",
            "memory_consolidate",
            "memory_update",
            "memory_supersede",
            "memory_delete",
            "memory_amend",
        ] {
            assert!(
                registry.get(name).is_some(),
                "'{name}' should be registered (memory is wired in make_factory)"
            );
        }
    }

    #[tokio::test]
    async fn sub_agent_has_plan_mutations_denied() {
        // Factory builds allow mutations by default (for main/parentless agents).
        // The spawner sets false for children; the flag is visible and the
        // plan tools (via wrapper + dispatch) deny.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let agent = factory.build();
        assert!(
            agent.plan_mutations_allowed(),
            "main builds allow plan tools"
        );

        // Simulate sub-agent construction (as IpcSpawner does for parent_id.is_some()).
        agent.set_plan_mutations_allowed(false);
        {
            let wfa = agent.workflow_handle();
            let mut wf = wfa.lock().await;
            wf.set_plan_mutations_allowed(false);
        }
        assert!(!agent.plan_mutations_allowed());

        // The create_plan tool itself now denies (defense in depth).
        let wf = agent.workflow_handle();
        let tool = CreatePlanTool::new(wf);
        let res = tool
            .execute(json!({"title":"T","goal":"G","context":"Sub-agent denial fixture: edit src/widget.rs and verify with cargo test.","steps":["edit src/widget.rs"]}))
            .await;
        assert!(!res.success);
        assert!(
            res.output.contains("restricted to the main agent"),
            "got: {}",
            res.output
        );
    }

    #[tokio::test]
    async fn build_with_id_and_plans_dir_uses_override() {
        // B1: a per-agent plans dir override flows into the workflow + loop,
        // but NOT into reviews_dir (which stays main-derived).
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let main_plans_dir = factory.plans_dir().to_path_buf();
        let per_agent_dir = main_plans_dir.join("agents/42");

        let agent = factory.build_with_id_and_plans_dir(42, per_agent_dir.clone());

        // The agent's workflow uses the per-agent dir.
        let wf = agent.workflow_handle();
        let wf_guard = wf.lock().await;
        assert_eq!(
            wf_guard.plans_dir(),
            per_agent_dir,
            "workflow must use the per-agent plans dir"
        );
        drop(wf_guard);

        // The factory's reviews_dir is still derived from the MAIN plans dir
        // (not the per-agent override) — so reviewer reports land at
        // .coding/reviews/, not .coding/plans/agents/reviews/.
        let reviews = factory.reviews_dir();
        assert!(
            reviews.ends_with("reviews"),
            "reviews_dir must be main-derived, got: {:?}",
            reviews
        );
        assert!(
            !reviews.starts_with(&per_agent_dir),
            "reviews_dir must NOT be under the per-agent dir"
        );
    }

    #[tokio::test]
    async fn build_with_root_spec_binds_tools_to_override_root() {
        // Parallel run-all (plan ffd7a86f): a worktree agent's file tools
        // must operate on the OVERRIDE root (its git worktree), not the
        // factory's project root — and the spec is stored on the loop so
        // subagents inherit the same root (spawn_agent_shared reads it).
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let worktree = tempfile::tempdir().unwrap();
        // Canonicalize so the sandbox's canonicalized root compares equal
        // (Windows `\\?\` prefixes).
        let worktree_root = worktree.path().canonicalize().unwrap();
        let plans_dir = worktree_root.join("plans/agents/42");
        let spec = AgentRootSpec {
            sandbox: Sandbox::new(&worktree_root).unwrap(),
            project_root: worktree_root.clone(),
            codegraph: None,
        };

        let agent = factory.build_with_root_spec(42, plans_dir.clone(), spec.clone());

        // The spec is stored on the loop (the spawner reads it so
        // subagents inherit the root).
        let stored = agent.root_spec().expect("root spec stored on the loop");
        assert_eq!(stored.sandbox.root(), worktree_root);
        assert_eq!(stored.project_root, worktree_root);
        assert!(stored.codegraph.is_none());

        // The workflow uses the worktree plans dir (plan bookkeeping lands
        // on the item's branch).
        let wf = agent.workflow_handle();
        let wf_guard = wf.lock().await;
        assert_eq!(wf_guard.plans_dir(), plans_dir);
        drop(wf_guard);

        // The registry's file tools bind to the OVERRIDE root: a write
        // lands in the worktree, never in the factory's project dir.
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(plans_dir.clone())));
        let registry = factory.build_registry(&workflow, Some(42), Some(&spec), &plans_dir);
        let tool = registry
            .iter()
            .find(|t| t.name() == "file_write")
            .expect("file_write registered");
        let result = tool
            .execute(serde_json::json!({"path": "probe.txt", "content": "x"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(
            worktree_root.join("probe.txt").exists(),
            "the write must land in the override (worktree) root"
        );
        assert!(
            !dir.path().join("probe.txt").exists(),
            "the write must NOT land in the factory's project root"
        );
    }

    #[tokio::test]
    async fn two_per_agent_dirs_dont_clobber() {
        // B1 isolation: two UI-spawned agents get distinct plans dirs, so a
        // plan created in one does NOT appear in the other's workflow.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let main_plans = factory.plans_dir().to_path_buf();

        let agent_a = factory.build_with_id_and_plans_dir(1, main_plans.join("agents/1"));
        let agent_b = factory.build_with_id_and_plans_dir(2, main_plans.join("agents/2"));

        // Create a plan in agent A's workflow directly.
        {
            let wf_a = agent_a.workflow_handle();
            let mut wf = wf_a.lock().await;
            wf.create_plan("A's plan", "goal", "ctx", vec!["step1".into()])
                .unwrap();
        }

        // Agent A is Executing; agent B must still be Planning (no plan).
        {
            let wf_a = agent_a.workflow_handle();
            let wf_b = agent_b.workflow_handle();
            let wf_a = wf_a.lock().await;
            let wf_b = wf_b.lock().await;
            assert_eq!(wf_a.state(), WorkflowState::Executing);
            assert_eq!(wf_b.state(), WorkflowState::Planning);
        }
    }

    #[tokio::test]
    async fn subagent_stamp_replaces_derived_state_keeps_mirror() {
        // 2026-01-03 restructure (backlog c5ded15d): a parented sub-agent's
        // workflow loads the MAIN plan from the shared plans dir (the
        // read-only mirror) but must NOT keep the derived lifecycle state —
        // the spawn path stamps WorkflowState::Subagent over it. This pins
        // the full sequence: load_latest derives Executing from the on-disk
        // plan, enter_subagent_state replaces it, the mirrored stack stays,
        // and the loop never expects progress.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        // Put an in-flight plan in the MAIN plans dir.
        {
            let wf = tokio::sync::Mutex::new(Workflow::new(
                factory.plans_dir().to_path_buf(),
            ));
            wf.lock()
                .await
                .create_plan("Main plan", "g", "c", vec!["s".into()])
                .unwrap();
        }
        // build_inner's load_latest derives Executing from the shared plan…
        let child = factory.build_with_id(9);
        {
            let handle = child.workflow_handle();
            let wf = handle.lock().await;
            assert_eq!(
                wf.state(),
                WorkflowState::Executing,
                "load_latest derives the main plan's state (the mirror load)"
            );
        }
        // …then the spawn path's parented stamps replace it (spawn.rs).
        {
            let handle = child.workflow_handle();
            let mut wf = handle.lock().await;
            wf.set_plan_mutations_allowed(false);
            wf.enter_subagent_state();
        }
        {
            let handle = child.workflow_handle();
            let wf = handle.lock().await;
            assert_eq!(wf.state(), WorkflowState::Subagent);
            assert!(
                wf.plan().is_some(),
                "the mirrored plan stack stays loaded (current_plan + UI staircase)"
            );
            assert!(!wf.plan_mutations_allowed());
        }
        assert!(
            // `false`: subagents are never run-all dispatched (the
            // unattended flag is derived from the MAIN agent's Prompt).
            !child.workflow_expects_progress(false).await,
            "a Subagent-state loop never expects progress (auto-continue parks)"
        );
    }
}
