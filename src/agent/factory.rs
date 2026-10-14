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

use crate::agent::context::ContextManager;
use crate::agent::{reflex::ReflexHandle, step_verify::StepVerifyHandle, AgentLoop, AgentLoopConfig};
use crate::config::{BudgetConfig, OptimizerConfig, SafetyMode, ShellFilterConfig, VerifyConfig};
use crate::memory::knowledge::{self, KnowledgeStore};
use crate::memory::MemoryStoreTrait;
use crate::project::ConstitutionSource;
use crate::provider::vision::{ImageDescriber, SwappableVision};
use crate::provider::{LlmClient, SwappableProvider};
use crate::runtime::AgentSpawner;
use crate::safety_rules::SafetyRules;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::agent::{
    convert_line_endings::ConvertLineEndingsTool,
    expand_result::ExpandResultTool,
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
    tool_choice::ToolChoiceHandle,
    web_fetch::WebFetchTool,
    write_review_report::WriteReviewReportTool,
};
use super::failure_triage::FailureTriageHandle;
use super::model_routing::RoutingGate;
use crate::tool::memory::retrieval::MemorySearchTool;
use crate::tool::memory::{
    AutoTypingHandle, MemoryAmendTool, MemoryConsolidateTool, MemoryDeleteTool,
    MemorySupersedeTool, MemoryUpdateTool, MemoryWriteTool,
};
use crate::tool::ToolRegistry;

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
/// The kernel tool groups (2c.2/G2, §5.9.2 :635-641): a
/// [`ToolSelection`] names the groups the factory registers.
/// `AskUser`/`Host` are surface slots for the app's and the Engine's
/// own tools (3a/3c); kernel-side only `Host` has a member today (the
/// reviewer channel), so a free-mode selection that excludes `Host`
/// drops `write_review_report` (row 5's letter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolGroup {
    /// The read surface: read_files, expand_result, search, search_read.
    Read,
    /// The write surface: file_edit, multi_edit, file_write,
    /// convert_line_endings (the spec's file_append is the file_write
    /// append mode — no separate kernel tool).
    Write,
    /// The shell.
    Shell,
    /// The git pair: git, git_read.
    Git,
    /// web_fetch.
    Web,
    /// The headless/on-screen browser tools (the `browser` feature).
    Browser,
    /// The graph_* tools.
    Codegraph,
    /// The memory_* tools.
    Memory,
    /// The image_* tools.
    Images,
    /// spawn_agent + list_models (§5.9.2 :641).
    Spawn,
    /// The app's ask_user (registered app-side; kernel-side no member
    /// post-2b).
    AskUser,
    /// The progressive-disclosure loader (load_tools) + the McpReveal
    /// wiring (§5.9.2 :641).
    Mcp,
    /// The reviewer channel (write_review_report) today; the Engine's
    /// host tools land in 3c.
    Host,
}

impl ToolGroup {
    /// Every group, in declaration order — [`ToolSelection::all`]'s
    /// include list.
    pub const ALL: &'static [ToolGroup] = &[
        ToolGroup::Read,
        ToolGroup::Write,
        ToolGroup::Shell,
        ToolGroup::Git,
        ToolGroup::Web,
        ToolGroup::Browser,
        ToolGroup::Codegraph,
        ToolGroup::Memory,
        ToolGroup::Images,
        ToolGroup::Spawn,
        ToolGroup::AskUser,
        ToolGroup::Mcp,
        ToolGroup::Host,
    ];
}

/// The per-agent tool-surface selection (2c.2/G2, §5.9.2 :635-642):
/// the kernel groups the factory registers + per-name carve-outs.
/// The workflow path passes NO selection — the factory's `None` field
/// registers every group in today's exact order (the byte-golden);
/// the Engine's SpawnOptions (3a) supplies the per-agent values.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolSelection {
    /// The groups to register (an empty list = no kernel groups).
    #[serde(default)]
    pub include: Vec<ToolGroup>,
    /// Per-name carve-outs (tools skipped inside the selected groups).
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl ToolSelection {
    /// Every group, no exclusions — the factory-level full set (what
    /// the `None` field means; §5.9.2 :642).
    pub fn all() -> Self {
        Self {
            include: ToolGroup::ALL.to_vec(),
            exclude: Vec::new(),
        }
    }

    /// The free agent's constant surface (§5.2's "the constant
    /// surface"): every kernel group EXCEPT `Host` — the reviewer
    /// channel is the workflow's, so a free agent advertises no review
    /// tool (row 5).
    pub fn free_default() -> Self {
        Self {
            include: ToolGroup::ALL
                .iter()
                .copied()
                .filter(|g| *g != ToolGroup::Host)
                .collect(),
            exclude: Vec::new(),
        }
    }

    /// Whether the group is included.
    pub fn includes(&self, group: ToolGroup) -> bool {
        self.include.contains(&group)
    }

    /// Whether the named tool rides the selection: its group must be
    /// included AND its name must not be excluded.
    pub fn selects(&self, group: ToolGroup, name: &str) -> bool {
        self.includes(group) && !self.exclude.iter().any(|e| e == name)
    }
}

/// The group-level gate (2c.2/G2, §5.9.2 :639: the factory registers
/// only the selected groups): `None` selection = full (today).
fn group_allowed(sel: Option<&ToolSelection>, group: ToolGroup) -> bool {
    match sel {
        None => true,
        Some(sel) => sel.includes(group),
    }
}

/// The app-side policy-factory slot (2b.2; master-spec :266-269) — a lock
/// whose CLONE is a SNAPSHOT (review F1, 3c.6): a derived factory clone
/// seeds its own FRESH lock from the current read, so a per-spawn
/// clone's swap (the engine's tier-1 named-policy arm) stays
/// clone-local and dies with the clone. The factory's other
/// interior-mutable seams (provider, context_manager, spawner,
/// descendant_tracker) stay SHARED through their Arcs on purpose — live
/// settings swaps must reach every clone's build — but the policy
/// factory is per-session state: a tier-1 spawn must never leak its
/// named factory into the base factory's slot (a later
/// plain/tier-2/tier-3 spawn would silently build under it, and
/// concurrent tier-1 spawns would race the one shared slot).
#[derive(Default)]
pub(crate) struct PolicyFactorySlot(
    RwLock<Option<Arc<dyn crate::agent::policy::AgentPolicyFactory>>>,
);

impl Clone for PolicyFactorySlot {
    fn clone(&self) -> Self {
        Self(RwLock::new(
            self.0
                .read()
                .expect("policy_factory lock poisoned")
                .clone(),
        ))
    }
}

impl PolicyFactorySlot {
    /// Swap the slot (the startup wiring; a per-spawn clone's tier-1 set).
    fn set(&self, factory: Option<Arc<dyn crate::agent::policy::AgentPolicyFactory>>) {
        *self.0.write().expect("policy_factory lock poisoned") = factory;
    }

    /// The slot's current factory (the build-time reads).
    fn get(&self) -> Option<Arc<dyn crate::agent::policy::AgentPolicyFactory>> {
        self.0
            .read()
            .expect("policy_factory lock poisoned")
            .clone()
    }
}

#[derive(Clone)]
pub struct AgentLoopFactory {
    /// The shared provider. Behind an `RwLock` so the model can be swapped at
    /// runtime (the status-bar model picker): the IPC layer builds a new client
    /// and swaps it in, and every subsequent `build()` uses it. Reads are cheap
    /// (one per `build()`).
    provider: Arc<RwLock<Arc<dyn LlmClient>>>,
    /// The DISPLAY-space effort of the shared default provider slot — stamped
    /// onto every loop built by this factory (the loops' no-override
    /// resolution branch reports it, backlog 51dab4da). Updated alongside
    /// `set_provider` swaps and initialized at startup from the resolved
    /// default model. `None` when unknown (test factories) — the UI falls
    /// back to the endpoint default.
    default_display_effort: Arc<RwLock<Option<String>>>,
    /// The context manager template. Swapped alongside the provider so the new
    /// model's context window takes effect for freshly built agents.
    context_manager: Arc<RwLock<ContextManager>>,
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
    /// The shared Laya tool-choice gate for `search` / `search_read` (backlog
    /// e2c47d5f) — `None` until the IPC layer wires it via `with_tool_choice`
    /// (the SAME shared classifier slot + the `steer_tool_choice` flag
    /// mirror). When not wired, both tools keep their regex heuristics.
    tool_choice: Option<ToolChoiceHandle>,
    /// The shared Laya failure-triage gate (backlog 1a4049c1) — `None` until
    /// the IPC layer wires it via `with_failure_triage` (the SAME shared
    /// classifier slot + the `failure_triage` flag mirror). Attached to every
    /// loop built after the call, so the tool-dispatch and provider retry
    /// layers read it at failure time.
    failure_triage: Option<FailureTriageHandle>,
    /// The shared Laya pre-prompt routing gate (backlog 091e694d) -- `None`
    /// until the IPC layer wires it via `with_routing_gate` (the SAME shared
    /// classifier slot; the opt-in flag + threshold ride the live config the
    /// resolver reads per turn). Attached to every loop built after the call,
    /// so each turn's pre-prompt classification reads it.
    routing: Option<RoutingGate>,
    /// The shared compound-reflex handle (backlog a8495cc1) -- the escalation
    /// lane ladder's classifier (backlog ad56c7bd). `None` until the IPC layer
    /// wires it via `with_reflex` (the SAME shared classifier slot + the
    /// `[general.laya] reflex` flag mirror, both read at every plan-step
    /// boundary). Attached to every loop built after the call.
    reflex: Option<ReflexHandle>,
    /// An optional spawner that lets an agent start background agents (the
    /// `spawn_agent` tool). `None` until the IPC layer wires it in via
    /// `set_spawner` — the tool is then omitted from the registry. Behind an
    /// `RwLock` so it can be set once at startup through the shared `Arc`.
    spawner: Arc<RwLock<Option<Arc<dyn AgentSpawner>>>>,
    /// An optional descendant tracker — the brain-side seam that lets the
    /// dispatch layer gate workflow-state transitions on "no spawned
    /// subagents running". `None` until the IPC layer wires it in via
    /// [`set_descendant_tracker`](Self::set_descendant_tracker). Behind an
    /// `RwLock` so it can be set once at startup through the shared `Arc`.
    descendant_tracker: Arc<RwLock<Option<Arc<dyn crate::runtime::DescendantTracker>>>>,
    /// The app-side policy factory (2b.2; master-spec :266-269). `None`
    /// (the default) builds kernel `NoPolicy`-equivalent policies via
    /// NoPolicyFactory — production and the kernel test helpers wire the
    /// app implementation via set_policy_factory before the first build.
    /// A [`PolicyFactorySlot`] — snapshot-cloned (review F1, 3c.6): a
    /// factory CLONE seeds its own fresh lock from the current read, so
    /// the engine's per-spawn tier-1 swap never leaks into the shared
    /// base factory (the Arc-shared live seams above stay shared by
    /// design; this one is per-session state).
    policy_factory: PolicyFactorySlot,

    /// An optional per-context model resolver. When present, every built agent
    /// gets it wired in so the turn driver can pick a model per workflow state
    /// / skill / subagent. `None` in tests / when no `[models]` section is
    /// configured. Set once at startup via `with_model_resolver`.
    model_resolver: Option<Arc<dyn crate::model_resolver::ModelResolver>>,
    /// The per-agent tool-surface selection (2c.2/G2, §5.9.2). `None` (the
    /// default) = every kernel group, today's exact registration order (the
    /// byte-golden). The Engine's SpawnOptions (3a) supplies per-agent
    /// selections at spawn; the workflow path never sets one.
    tool_selection: Option<ToolSelection>,
    /// The steering counters every built loop shares (2c.4/G4, §5.9.4):
    /// one instance per factory, cloned into each loop via
    /// `AgentLoop::with_steering` — the de-globalized replacement for the
    /// deleted process-wide `SteeringStats::shared()` singleton. Read by
    /// the IPC layer via [`steering`](Self::steering).
    steering: Arc<crate::agent::steering_stats::SteeringStats>,
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
    /// Live mirror of `[general.optimizer]` (token-optimizer levers,
    /// backlog e4a50d22), shared with every lever-bearing tool built from
    /// this factory (read_files delta/skeleton re-reads, shell output
    /// compression, …). Defaults to all-off; the IPC layer overrides via
    /// [`with_optimizer_config`](Self::with_optimizer_config) from the
    /// loaded config at startup (mirrors `shell_filter`).
    optimizer: Arc<RwLock<OptimizerConfig>>,
    /// Live mirror of `[general.verify]` (harness-run deterministic verify at
    /// plan-step boundaries — the cost-saving chain's verification item),
    /// shared with every `StepVerifyHandle` this factory builds. Defaults to
    /// the inert config (disabled, no command, zero spawns); the IPC layer
    /// overrides via [`with_verify_config`](Self::with_verify_config) from
    /// the loaded config at startup (mirrors `optimizer`).
    verify: Arc<RwLock<VerifyConfig>>,
    /// The shared `[general.budget]` config (backlog a25a5323, the
    /// cost-saving chain's budget item): every loop built from this factory
    /// reads it at its plan-step gates, so a Settings save is observed on the
    /// next boundary with no rebuild (mirrors `verify`).
    budget: Arc<RwLock<BudgetConfig>>,
    /// The persistent index-staleness log (backlog fc1d57fe) handed to every
    /// search/graph tool this factory builds: each stale-index repair appends
    /// one record per file, and the freshness note points at it. Defaults to
    /// the global config dir; tests override with
    /// [`with_staleness_log`](Self::with_staleness_log) so no test writes the
    /// real one.
    staleness_log: crate::index_staleness::StalenessLog,
}

impl AgentLoopFactory {
    /// The factory's steering counters (2c.4/G4) — the instance-scoped
    /// replacement for the deleted process-wide singleton. The IPC
    /// `get_steering_stats` command reads this; every loop this factory
    /// builds reports here.
    pub fn steering(&self) -> &Arc<crate::agent::steering_stats::SteeringStats> {
        &self.steering
    }

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
            provider: Arc::new(RwLock::new(provider.clone())),
            default_display_effort: Arc::new(RwLock::new(None)),
            context_manager: Arc::new(RwLock::new(context_manager)),
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
            steering: Arc::new(crate::agent::steering_stats::SteeringStats::new()),
            vision: SwappableVision::new(vision),
            // The provider slot starts populated with the initial provider so
            // the `memory_consolidate` tool can run full extraction on its
            // first call. `set_provider` keeps this slot in sync with swaps.
            provider_slot: SwappableProvider::new(Some(provider)),
            // No spawner at construction — the IPC layer wires it in via
            // `set_spawner` once the `AgentManager` exists. Until then the
            // `spawn_agent` tool is omitted from every registry.
            spawner: Arc::new(RwLock::new(None)),
            // No descendant tracker at construction — the IPC layer wires it
            // in via `set_descendant_tracker` alongside the spawner. Until
            // then the state-transition gate is not enforced.
            descendant_tracker: Arc::new(RwLock::new(None)),
            policy_factory: PolicyFactorySlot::default(),
            // No skill registry at construction — the IPC layer wires it in

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
            // Optimizer config at its default (every lever ON, 2027-01-25);
            // the IPC layer overrides via `with_optimizer_config` from the
            // loaded config at startup (mirrors `shell_filter`).
            optimizer: Arc::new(RwLock::new(OptimizerConfig::default())),
            // Verify config at its inert default (disabled — no command, no
            // spawn); the IPC layer overrides via `with_verify_config` from
            // the loaded config at startup (mirrors `optimizer`).
            verify: Arc::new(RwLock::new(VerifyConfig::default())),
            // Budget config at its inert default (disabled — no cap ever
            // fires); the IPC layer overrides via `with_budget_config` from
            // the loaded config at startup (mirrors `verify`).
            budget: Arc::new(RwLock::new(BudgetConfig::default())),
            // The global index-staleness log (backlog fc1d57fe) unless a test
            // overrides it.
            staleness_log: crate::index_staleness::StalenessLog::global(),
            mcp: None,
            // No auto-typing gate at construction — the IPC layer wires it
            // via `with_auto_typing` once the app runtime's classifier slot
            // exists (backlog a147b63c). Until then `memory_write` never
            // asks a classifier.
            typing: None,
            // No failure-triage gate at construction either — wired via
            // `with_failure_triage` (backlog 1a4049c1). Until then every
            // failure-handling site keeps its pre-classifier behavior.
            failure_triage: None,
            routing: None,
            // No compound-reflex handle at construction either — wired via
            // `with_reflex` (backlog a8495cc1). Until then the escalation lane
            // ladder (backlog ad56c7bd) never classifies.
            reflex: None,
            // No tool-choice gate at construction — wired via
            // `with_tool_choice` (backlog e2c47d5f). Until then the search
            // tools keep their regex auto-delegation heuristics byte-identically.
            tool_choice: None,
            // No tool-surface selection at construction (2c.2/G2): every
            // kernel group registers — today's exact registration order.
            // The Engine (3a) supplies per-agent selections at spawn.
            tool_selection: None,
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

    /// Wire the shared Laya tool-choice gate (backlog e2c47d5f): the `search`
    /// and `search_read` tools read the shared classifier slot + the
    /// `[general.laya] steer_tool_choice` flag at call time. When not wired,
    /// both tools keep their regex auto-delegation heuristics.
    pub fn with_tool_choice(mut self, handle: ToolChoiceHandle) -> Self {
        self.tool_choice = Some(handle);
        self
    }

    /// Flip the tool-choice enable flag on the shared gate — the Settings save
    /// path (rewire) calls this so the toggle reaches already-built tools with
    /// no registry rebuild.
    pub fn set_tool_choice_enabled(&self, on: bool) {
        if let Some(handle) = &self.tool_choice {
            handle
                .enabled
                .store(on, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Set the per-agent tool-surface selection (2c.2/G2, §5.9.2): the
    /// kernel groups the factory registers for agents built after this
    /// call. `None` (the default) = every group, today's exact order. The
    /// workflow path never sets one.
    pub fn with_tool_selection(mut self, selection: ToolSelection) -> Self {
        self.tool_selection = Some(selection);
        self
    }

    /// Wire the shared Laya failure-triage gate (backlog 1a4049c1): every
    /// agent loop built after this call carries the handle, whose shared
    /// classifier slot + `[general.laya] failure_triage` flag mirror are read
    /// at FAILURE time — so a Settings save needs no rebuild. When not wired,
    /// every failure-handling site keeps its pre-classifier behavior.
    pub fn with_failure_triage(mut self, handle: FailureTriageHandle) -> Self {
        self.failure_triage = Some(handle);
        self
    }

    /// Wire the shared Laya pre-prompt routing gate (backlog 091e694d): every
    /// agent loop built after this call carries the handle, whose shared
    /// classifier slot is read at turn start. The `[general.laya] routing`
    /// opt-in and the `[general.routing]` threshold / `enforce` ride the LIVE
    /// config the model resolver reads per turn, so a Settings save needs no
    /// rebuild (and there is no flag mirror to keep in sync). When not wired,
    /// no turn ever classifies.
    pub fn with_routing_gate(mut self, gate: RoutingGate) -> Self {
        self.routing = Some(gate);
        self
    }

    /// Wire the shared compound-reflex handle (backlog a8495cc1) -- the
    /// escalation lane ladder's classifier (backlog ad56c7bd). Every loop built
    /// after this call carries the handle, whose shared classifier slot + the
    /// `[general.laya] reflex` flag mirror are read at every plan-step
    /// boundary, so a Settings save needs no rebuild. When not wired, the
    /// ladder never classifies and model selection is byte-identical.
    pub fn with_reflex(mut self, handle: ReflexHandle) -> Self {
        self.reflex = Some(handle);
        self
    }

    /// Flip the failure-triage enable flag on the shared gate — the Settings
    /// save path (rewire) calls this so the toggle reaches already-built
    /// loops with no rebuild.
    pub fn set_failure_triage_enabled(&self, on: bool) {
        if let Some(handle) = &self.failure_triage {
            handle
                .enabled
                .store(on, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Flip the kNN overlay's enable flag on the shared gate — the Settings
    /// save path (rewire) calls this after `[general.laya]
    /// failure_triage_knn` changes, so the toggle reaches already-built
    /// loops with no rebuild. No-op when no gate is wired (the overlay is
    /// then absent and the pre-classifier behavior stands).
    pub fn set_failure_triage_knn_enabled(&self, on: bool) {
        if let Some(handle) = &self.failure_triage {
            handle
                .knn_enabled
                .store(on, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Flip the compound-reflex enable flag on the shared handle -- the
    /// Settings save path (rewire) calls this so the `[general.laya] reflex`
    /// toggle reaches already-built loops with no rebuild. No-op when no
    /// handle is wired.
    pub fn set_reflex_enabled(&self, on: bool) {
        if let Some(handle) = &self.reflex {
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

    /// Point the index-staleness log at an explicit directory (backlog
    /// fc1d57fe) — tests pass a tempdir so no test writes the real config
    /// dir; production keeps the global default.
    pub fn with_staleness_log(mut self, log: crate::index_staleness::StalenessLog) -> Self {
        self.staleness_log = log;
        self
    }


    /// The knowledge-store handle (2b.3): the bin arms construct the app
    /// policy factory from this accessor — the plan/skill registrations it
    /// owns carry the same wiring the kernel registrations had.
    pub fn knowledge_handle(&self) -> Option<Arc<KnowledgeStore>> {
        self.knowledge.clone()
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

    /// Wire the shared `[general.optimizer]` config (token-optimizer
    /// levers, backlog e4a50d22). Called once by the IPC layer at startup;
    /// every lever-bearing tool built after this call shares the same
    /// `Arc<RwLock<OptimizerConfig>>` (mirrors
    /// [`with_shell_filter_config`](Self::with_shell_filter_config)).
    pub fn with_optimizer_config(mut self, cfg: Arc<RwLock<OptimizerConfig>>) -> Self {
        self.optimizer = cfg;
        self
    }

    /// Wire the shared `[general.verify]` config (harness-run deterministic
    /// verify at plan-step boundaries — the cost-saving chain's verification
    /// item). Called once by the IPC layer at startup; every
    /// [`StepVerifyHandle`] built from this factory shares the same
    /// `Arc<RwLock<VerifyConfig>>`, so a Settings save is observed at the next
    /// step boundary with no rebuild (mirrors
    /// [`with_optimizer_config`](Self::with_optimizer_config)).
    pub fn with_verify_config(mut self, cfg: Arc<RwLock<VerifyConfig>>) -> Self {
        self.verify = cfg;
        self
    }

    /// Wire the shared `[general.budget]` config (the cost-saving chain's
    /// budget item, backlog a25a5323). Called once by the IPC layer at
    /// startup; every loop built from this factory shares the same
    /// `Arc<RwLock<BudgetConfig>>` and reads it at its plan-step gates, so a
    /// Settings save is observed on the next gate with no rebuild (mirrors
    /// [`with_verify_config`](Self::with_verify_config)).
    pub fn with_budget_config(mut self, cfg: Arc<RwLock<BudgetConfig>>) -> Self {
        self.budget = cfg;
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

    /// Wire the app-side policy factory (2b.2; master-spec :266-269: "the
    /// factory takes `Arc<dyn PolicyFactory>`"). Call before the first
    /// `build()` — production wires at startup; the kernel test helpers wire
    /// so factory-built agents keep the 2a-era WorkflowPolicy semantics with
    /// no per-test attach. Unwired factories build the kernel NoPolicy
    /// default.
    pub fn set_policy_factory(
        &self,
        factory: Arc<dyn crate::agent::policy::AgentPolicyFactory>,
    ) {
        self.policy_factory.set(Some(factory));
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

    /// Update the live `[general.optimizer]` config (called by the app's
    /// `save_settings` / `save_endpoints` rewire after the config is persisted
    /// and reloaded). Every lever-bearing tool built from this factory shares
    /// the same `Arc<RwLock<OptimizerConfig>>` (see
    /// [`with_optimizer_config`](Self::with_optimizer_config)) and reads it per
    /// call, so a Settings toggle lands on the next tool call — no registry
    /// rebuild and no app restart.
    pub fn set_optimizer_config(&self, cfg: OptimizerConfig) {
        *self.optimizer.write().expect("optimizer lock poisoned") = cfg;
    }

    /// Update the live `[general.verify]` config (called by the app's
    /// `save_settings` / `save_endpoints` rewire after the config is persisted
    /// and reloaded). Every [`StepVerifyHandle`] shares the same
    /// `Arc<RwLock<VerifyConfig>>` (see
    /// [`with_verify_config`](Self::with_verify_config)) and reads it per
    /// plan-step boundary, so a Settings save lands on the next
    /// `complete_step` — no registry rebuild and no app restart.
    pub fn set_verify_config(&self, cfg: VerifyConfig) {
        *self.verify.write().expect("verify lock poisoned") = cfg;
    }

    /// Update the live `[general.budget]` config (called by the app's
    /// `save_settings` / `save_endpoints` rewire after the config is
    /// persisted and reloaded). Every loop shares the same
    /// `Arc<RwLock<BudgetConfig>>` (see
    /// [`with_budget_config`](Self::with_budget_config)) and reads it at each
    /// plan-step gate, so a Settings save lands on the next gate — no
    /// registry rebuild and no app restart.
    pub fn set_budget_config(&self, cfg: BudgetConfig) {
        *self.budget.write().expect("budget lock poisoned") = cfg;
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
        // 2b.3: the workflow construction moved app-side (the kernel never
        // names the app type): the wired factory's create_workflow builds
        // it — loading the latest plan from disk so a newly-spawned agent
        // resumes an in-flight plan, and stamping the Subagent state when
        // the loop's own flag says so (the spawn path's belt-and-braces
        // post-build stamp rides the app side through the same handle). The
        // unwired (NoPolicy) factories get the kernel UnwiredWorkflow —
        // every consult reads the neutral defaults.
        let factory_slot0 = self.policy_factory.get();
        let workflow: Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle> =
            match factory_slot0.as_ref() {
                Some(pf) => pf.create_workflow(&plans_dir, false),
                None => Arc::new(crate::agent::workflow_handle::UnwiredWorkflow),
            };
        drop(factory_slot0);

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
        // Share the live `[general.optimizer]` handle (backlog e4a50d22) so
        // the ingestion-time levers (progressive disclosure, compaction
        // survival) see a config save without rebuilding the loop.
        .with_optimizer(Arc::clone(&self.optimizer))
        // Wire the plans dir so session-end consolidation can digest the
        // accumulated plan/review corpus (learning material). Uses the
        // per-build override (main dir or per-agent dir).
        .with_plans_dir(plans_dir.clone())
        // G4 (2c.4): every loop this factory builds shares its steering
        // counters — the instance-scoped metric.
        .with_steering(Arc::clone(&self.steering))
        // 2c.5 (§5.4): GUI/console loops keep today's approval
        // semantics — ApproveViaApi over the factory's shared mode.
        .with_permission(
            crate::agent::policy::PermissionPolicy::ApproveViaApi {
                safety_mode: *self.safety_mode.read().expect("safety_mode lock poisoned"),
            },
        );

        // 2b.2: the agent's policy comes from the app-side policy factory
        // (master-spec :266-269) — the kernel never names the app policy
        // type; unwired factories build the kernel NoPolicy default. Since
        // 2a.3 the kernel consults the policy (the tool surface, the
        // volatile tail, the prompt/turn-end hooks, and — 2a.6 — the
        // routing key). The returned attach hook runs at the build's tail
        // once the loop is Arc'd.
        let built_policy = {
            let factory_slot = self.policy_factory.get();
            match factory_slot.as_ref() {
                Some(pf) => {
                    let ctx = crate::agent::policy::PolicyFactoryCtx {
                        agent_id,
                        project_root: self.project_root.clone(),
                        plans_dir,
                        // The handle passes straight through — the ctx field
                        // IS the neutral trait type since 2b.3 (no cast).
                        workflow: agent.workflow_handle(),
                        // 2c.1 (G1): the caller's prompt spec arrives with
                        // the Engine's SpawnOptions (3a). The app factory
                        // (production's wiring) composes today's workflow
                        // head and ignores the option.
                        prompt: None,
                    };
                    // 2c.1 (G1, §5.9.1 :629-630): a wired factory may REFUSE
                    // the build — today only a NoPolicy factory without the
                    // caller's PromptSpec. The loop build is infallible and
                    // production always wires the app factory (which never
                    // errs), so the kernel runs the EXPLICIT coding-default
                    // fallback here; the Engine path (3a) owns propagating
                    // the error to its spawn callers (step-file D2's
                    // documented forward).
                    pf.build_policy(&ctx).unwrap_or_else(|_error| {
                        crate::agent::policy::PolicyBuildResult {
                            policy: Arc::new(
                                crate::agent::policy::NoPolicy::with_coding_default(),
                            ),
                            attach_loop: Box::new(|_| {}),
                        }
                    })
                }
                None => {
                    // 2c.1 (G1): the unwired arm's default is the kernel's
                    // EXPLICIT coding default (never a silent assembly).
                    crate::agent::policy::PolicyBuildResult {
                        policy: Arc::new(
                            crate::agent::policy::NoPolicy::with_coding_default(),
                        ),
                        attach_loop: Box::new(|_| {}),
                    }
                }
            }
        };
        agent.set_policy(built_policy.policy);

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

        // Attach the failure-triage gate (backlog 1a4049c1): the loop's
        // tool-dispatch site and both provider retry layers read the shared
        // classifier slot + the `[general.laya] failure_triage` flag at
        // failure time, so a Settings save lands on the next failure with no
        // registry or loop rebuild.
        if let Some(handle) = &self.failure_triage {
            agent = agent.with_failure_triage(handle.clone());
        }

        // Attach the pre-prompt routing gate (backlog 091e694d): each turn's
        // pre-prompt classification reads the shared classifier slot; the
        // `[general.laya] routing` flag and the `[general.routing]` threshold /
        // enforce ride the live config the resolver reads per turn, so a
        // Settings save lands on the next turn with no rebuild.
        if let Some(gate) = &self.routing {
            agent = agent.with_routing_gate(gate.clone());
        }

        // Attach the compound-reflex handle (backlog a8495cc1) -- the
        // escalation lane ladder's classifier (backlog ad56c7bd): each
        // plan-step boundary reads the shared classifier slot + the
        // `[general.laya] reflex` flag mirror, so a Settings save lands on the
        // next step with no rebuild.
        if let Some(handle) = &self.reflex {
            agent = agent.with_reflex(handle.clone());
        // Harness-run deterministic verify (the cost-saving chain's
        // verification item): the loop re-runs `[general.verify]`'s command on
        // a decided `action=verify` reflex call and rides the compact evidence
        // note on the next request's volatile tail. Same live config the
        // `complete_step` tool reads (`step_verify_handle`).
        agent = agent.with_step_verify(self.step_verify_handle(root));
        }

        // The budget layer (backlog a25a5323): every loop reads the shared
        // live `[general.budget]` config at its plan-step gates — a Settings
        // save lands on the next gate with no rebuild. Unconditional: the cap
        // gate works with the reflex handle absent (caps are deterministic,
        // not classifier-driven).
        agent = agent.with_budget_config(Arc::clone(&self.budget));

        // Stamp the shared default's DISPLAY effort (backlog 51dab4da): the
        // loop's no-override resolution branch reports it, so the status bar
        // shows the default model's effective effort (per-model override →
        // endpoint default → "max") — never just the endpoint-level default.
        // None for test factories (the UI falls back).
        agent.set_default_display_effort(self.default_display_effort());

        let agent = Arc::new(agent);
        // 2a.6: attach the loop back-ref — the policy's subagent-slot source
        // (idempotent OnceLock; the first attach wins).
        (built_policy.attach_loop)(Arc::downgrade(&agent));
        agent
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
        workflow: &Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle>,
        agent_id: Option<crate::runtime::AgentId>,
        root: Option<&AgentRootSpec>,
        plans_dir: &Path,
    ) -> ToolRegistry {
        // 2c.2 (G2): the tool-surface selection — None = every kernel
        // group, today's exact registration order (the byte-golden).
        let selection = self.tool_selection.as_ref();
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
        self.register_agent_tools(&mut registry, &sandbox, root, selection);
        // 2b.2/2b.3: the app-owned tools (the plan/skill sets moved in 2b.3,
        // ask_user + the backlog trio) register via the policy factory — D5:
        // kernel-called, app-owned. The delegate sits at the position the
        // plan-tool block held, and the app impl preserves today's relative
        // order (plan tools, ask_user, backlog, skill tools), so the
        // registry's insertion ORDER stays byte-stable. NOT G2-gated: the
        // app surface is the app factory's own (the workflow path = today's
        // set). The app-side group honoring lands with the Engine surface
        // (3a/3c).
        self.register_policy_factory_tools(&mut registry, agent_id, root, plans_dir, &sandbox, workflow);
        self.register_vision_tool(&mut registry, &sandbox, selection);
        self.register_memory_tools(&mut registry, root, selection);
        self.register_codegraph_tools(&mut registry, root, selection);
        // The reviewer-round scope wiring (backlog 85313a7e) needs the project
        // root the git reads run in — resolved the way `register_agent_tools`
        // resolves it (the agent's own root when it has one, else the
        // factory's).
        let project_root = match root {
            Some(spec) => spec.project_root.clone(),
            None => self.project_root.clone(),
        };
        self.register_spawn_tool(&mut registry, workflow, &project_root, agent_id, selection);
        #[cfg(feature = "browser")]
        self.register_browser_tools(&mut registry, &sandbox, selection);
        // The reveal half of progressive disclosure — registered last so it
        // shares the reveal-set the registry was built with. MCP groups
        // reveal through the shared manager into this registry's dynamic
        // tool slot (the same shared-handle pattern as the reveal-set).
        let mcp_reveal = self.mcp.as_ref().map(|manager| {
            crate::mcp::McpReveal::new(std::sync::Arc::clone(manager), registry.dynamic_slot())
        });
        // 2c.2 (G2): the caller's exclude = a per-name carve-out on the WHOLE
        // array (§5.9.2 :639-641: the tool array never advertises tools the
        // caller excluded) — pruned after every registration (the app's
        // included), BEFORE the deferred snapshot, so excluded tools neither
        // advertise nor reveal.
        if let Some(sel) = selection {
            for name in &sel.exclude {
                registry.unregister(name);
            }
        }
        // The static deferred tools' schemas render in the reveal response —
        // snapshot them (Arc clones) before LoadToolsTool registers; static
        // tools never change at runtime.
        let deferred = Self::deferred_snapshot(&registry);
        // 2c.2 (G2): the reveal loader rides the `mcp` group (§5.9.2 :641:
        // load_tools registered only with Mcp) — a selected surface without
        // Mcp gets no loader, and its deferred groups never reveal.
        if group_allowed(selection, ToolGroup::Mcp) {
            registry.register(Box::new(
                crate::tool::agent::load_tools::LoadToolsTool::new(
                    registry.loaded_groups(),
                    mcp_table,
                    mcp_reveal,
                    deferred,
                ),
            ));
        }
        registry
    }

    /// The reviews directory (`.coding/reviews/`), derived from `plans_dir`
    /// the same way `FinishTool` derives it at runtime
    /// (`plans_dir.parent().join("reviews")`). Used to construct the
    /// `write_review_report` tool so the reviewer subagent's single output
    /// channel writes to the same dir `finish` later validates. G3 (2c.3):
    /// the plans-dir-less fallback is root-derived, never CWD-relative.
    pub fn reviews_dir(&self) -> std::path::PathBuf {
        self.plans_dir
            .parent()
            .map(|p| p.join("reviews"))
            .unwrap_or_else(|| {
                self.project_root
                    .join(crate::workspace::CODING_DIR_NAME)
                    .join("reviews")
            })
    }

    /// Register the file/shell/search/git agent tools (always present). These
    /// are the coding tools the agent uses to do work; they are gated by the
    /// workflow's `ToolFilter` at dispatch time, not at registration.
    fn register_agent_tools(
        &self,
        registry: &mut ToolRegistry,
        sandbox: &Sandbox,
        root: Option<&AgentRootSpec>,
        sel: Option<&ToolSelection>,
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
        // 2c.2 (G2): the per-name gate — None selection = register (today's
        // order); some = the name's group must be included and the name not
        // excluded. The app-owned policy-factory tools are NOT gated here
        // (that surface is the app factory's own).
        let want = |group: ToolGroup, name: &str| match sel {
            None => true,
            Some(sel) => sel.selects(group, name),
        };
        if want(ToolGroup::Read, "read_files") {
            registry.register(Box::new(
                ReadFilesTool::new(sandbox.clone())
                    .with_codegraph(codegraph.clone())
                    .with_optimizer(Arc::clone(&self.optimizer)),
            ));
        }
        // Progressive disclosure (backlog e4a50d22 lever 3): serve back a
        // tool result that was archived for size — by id, or by keyword. A
        // `None` store just yields a helpful error on call.
        if want(ToolGroup::Read, "expand_result") {
            registry.register(Box::new(ExpandResultTool::new(self.memory.clone())));
        }
        if want(ToolGroup::Write, "file_edit") {
            registry.register(Box::new(FileEditTool::new(sandbox.clone())));
        }
        if want(ToolGroup::Write, "multi_edit") {
            registry.register(Box::new(MultiEditTool::new(sandbox.clone())));
        }
        if want(ToolGroup::Write, "file_write") {
            registry.register(Box::new(FileWriteTool::new(sandbox.clone())));
        }
        if want(ToolGroup::Write, "convert_line_endings") {
            registry.register(Box::new(ConvertLineEndingsTool::new(sandbox.clone())));
        }
        if want(ToolGroup::Shell, "shell") {
            registry.register(Box::new(
                ShellTool::new(sandbox.clone())
                    .with_filter_config(Arc::clone(&self.shell_filter))
                    // Same handle as GitTool: `shell git merge|push` must raise the
                    // always-on core-operation prompt too (follow-up A, 2027-01-11).
                    .with_core_operations(Arc::clone(&self.core_operations))
                    // Token-optimizer lever 2 (backlog e4a50d22): the shared
                    // `[general.optimizer]` handle, read per call so a config
                    // save lands on the next command without a rebuild.
                    .with_optimizer(Arc::clone(&self.optimizer)),
            ));
        }
        let mut search_tool = SearchTool::new(sandbox.clone(), codegraph.clone())
            .with_staleness_log(self.staleness_log.clone());
        let mut search_read_tool = SearchReadTool::new(sandbox.clone(), codegraph.clone())
            .with_staleness_log(self.staleness_log.clone());
        // F9: the memory store powers the known-memory-hit note on
        // uuid-shaped search patterns.
        if let Some(store) = &self.memory {
            search_tool = search_tool.with_memory(store.clone());
            search_read_tool = search_read_tool.with_memory(store.clone());
        }
        // Laya tool-choice steering (backlog e2c47d5f, opt-in): the shared
        // classifier slot + the `steer_tool_choice` flag mirror, read per call
        // so a Settings save lands on the next search without a rebuild.
        // Unwired → the regex heuristics run byte-identically.
        if let Some(handle) = &self.tool_choice {
            search_tool = search_tool.with_tool_choice(handle.clone());
            search_read_tool = search_read_tool.with_tool_choice(handle.clone());
        }
        if want(ToolGroup::Read, "search") {
            registry.register(Box::new(search_tool));
        }
        if want(ToolGroup::Read, "search_read") {
            registry.register(Box::new(search_read_tool));
        }
        if want(ToolGroup::Web, "web_fetch") {
            registry.register(Box::new(WebFetchTool::new()));
        }
        // 2c.3 (G3): the Git group also requires a repo at the root (row 27's
        // kernel form) — a non-repo directory builds without git git_read.
        if want(ToolGroup::Git, "git")
            && crate::workspace::Workspace::root_is_git_repo(&project_root)
        {
            registry.register(Box::new(GitTool::with_core_operations(
                &project_root,
                self.core_operations.clone(),
            )));
        }
        // git_read is the read-only view into git (op = diff | log | show | status):
        // the reviewer subagent's only way to see uncommitted changes (it has
        // no shell/git), and the bridge from a memory record's commit pointer
        // to shipped code. AutoRun by construction, which is exactly why it
        // stays separate from the NeedsApproval `git` tool — it must remain
        // visible in the read-only Planning/Complete states.
        // 2c.3 (G3): the same repo-presence gate as `git`.
        if want(ToolGroup::Git, "git_read")
            && crate::workspace::Workspace::root_is_git_repo(&project_root)
        {
            registry.register(Box::new(GitReadTool::new(&project_root)));
        }
        // write_review_report is the reviewer subagent's single output channel
        // (writes only under .coding/reviews/). Always registered in the FULL
        // set; gated by the workflow ToolFilter, and built through the single
        // construction point below so its provenance wiring cannot drift from
        // its test. 2c.2 (G2, row 5): the reviewer channel rides the Host
        // group — a free-mode selection without `Host` advertises no review
        // tool.
        if want(ToolGroup::Host, "write_review_report") {
            registry.register(Box::new(self.build_write_review_report_tool(&project_root)));
        }
        // list_models is read-only discovery (which models are configured) so
        // the agent can pick one for spawn_agent's `model` parameter. Omitted
        // when no model resolver is wired (tests / no `[models]` section) —
        // there's nothing to list. 2c.2 (G2, §5.9.2 :641): registered only
        // with the Spawn group.
        if want(ToolGroup::Spawn, "list_models") {
            if let Some(resolver) = &self.model_resolver {
                registry.register(Box::new(ListModelsTool::new(Some(Arc::clone(resolver)))));
            }
        }
    }

    /// Build the harness-run verification handle for one agent (see
    /// [`crate::agent::step_verify`]). Every handle reads the SAME live
    /// `[general.verify]` + `[general.optimizer]` config, so the
    /// `complete_step` tool and the loop's action=verify path always agree;
    /// the root is the agent's sandbox (a root-spec agent verifies inside its
    /// own worktree).
    fn step_verify_handle(&self, root: Option<&AgentRootSpec>) -> StepVerifyHandle {
        let root = match root {
            Some(spec) => spec.sandbox.root().to_path_buf(),
            None => self.sandbox.root().to_path_buf(),
        };
        StepVerifyHandle::new(Arc::clone(&self.verify), Arc::clone(&self.optimizer)).with_root(root)
    }


    /// Register the app-owned tools via the policy factory (2b.2; D5:
    /// kernel-called, app-owned). The app impl does the registering; the
    /// kernel factory never names the app's wiring or policy types. No-op
    /// without a wired factory — unwired (NoPolicy) factories build
    /// registries without the app-owned tools.
    fn register_policy_factory_tools(
        &self,
        registry: &mut ToolRegistry,
        agent_id: Option<crate::runtime::AgentId>,
        root: Option<&AgentRootSpec>,
        plans_dir: &Path,
        sandbox: &Sandbox,
        workflow: &Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle>,
    ) {
        let slot = self.policy_factory.get();
        let ctx = crate::agent::policy::PolicyBuildCtx {
            agent_id,
            project_root: self.project_root.clone(),
            plans_dir: plans_dir.to_path_buf(),
            root: root.cloned(),
            sandbox: sandbox.clone(),
            step_verify: self.step_verify_handle(root),
            workflow: Arc::clone(workflow),
        };
        if let Some(pf) = slot.as_ref() {
            pf.register_tools(registry, &ctx);
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
    fn register_browser_tools(
        &self,
        registry: &mut ToolRegistry,
        sandbox: &Sandbox,
        sel: Option<&ToolSelection>,
    ) {
        if !group_allowed(sel, ToolGroup::Browser) {
            // 2c.2 (G2): the selection excludes the Browser group.
            return;
        }
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
    fn register_vision_tool(
        &self,
        registry: &mut ToolRegistry,
        sandbox: &Sandbox,
        sel: Option<&ToolSelection>,
    ) {
        if !group_allowed(sel, ToolGroup::Images) {
            // 2c.2 (G2): the selection excludes the Images group.
            return;
        }
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
    fn register_memory_tools(
        &self,
        registry: &mut ToolRegistry,
        root: Option<&AgentRootSpec>,
        sel: Option<&ToolSelection>,
    ) {
        if !group_allowed(sel, ToolGroup::Memory) {
            // 2c.2 (G2): the selection excludes the Memory group.
            return;
        }
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
        sel: Option<&ToolSelection>,
    ) {
        if !group_allowed(sel, ToolGroup::Codegraph) {
            // 2c.2 (G2): the selection excludes the Codegraph group.
            return;
        }
        // A root-spec agent's graph_* tools bind to the worktree's graph.
        let graph = match root {
            Some(spec) => spec.codegraph.clone(),
            None => self.codegraph.clone(),
        };
        if let Some(graph) = &graph {
            use crate::tool::agent::codegraph::{
                GraphContextTool, GraphImpactTool, GraphPathTool, GraphSearchTool,
            };
            registry.register(Box::new(
                GraphSearchTool::new(graph.clone()).with_staleness_log(self.staleness_log.clone()),
            ));
            registry.register(Box::new(GraphContextTool::new(graph.clone())));
            registry.register(Box::new(GraphImpactTool::new(graph.clone())));
            registry.register(Box::new(GraphPathTool::new(graph.clone())));
        }
    }

    /// The `write_review_report` tool with its provenance wiring attached
    /// (backlog 85313a7e): a freshly created report then ends with the revision
    /// the round reviewed.
    ///
    /// A SINGLE construction point, shared by the registration in
    /// `register_agent_tools` and the reviewer-tools wiring test — the stamp's
    /// behaviour needs a real git repository to witness (pinned instead by the
    /// `FakeScoper` tests in the tool's own module), so the REGISTRATION is
    /// asserted structurally. Dropping `with_review_scoper` here reverts the
    /// stamp and fails that test (review round 1, LOW 3).
    fn build_write_review_report_tool(&self, project_root: &Path) -> WriteReviewReportTool {
        WriteReviewReportTool::new(self.reviews_dir()).with_review_scoper(
            Arc::new(crate::agent::review_scope::GitReviewScoper),
            project_root.to_path_buf(),
        )
    }

    /// Register the spawn-agent tool once the IPC layer has wired in a
    /// spawner. Without one there's no way to start a background agent, so the
    /// tool is omitted entirely (rather than erroring at call time). When this
    /// agent has a runtime id, the tool is made parent-aware so background
    /// agents it spawns are registered as its children (closing the
    /// completion-notification feedback loop). The per-agent `workflow` and
    /// `project_root` are wired in as the review-round scope (backlog
    /// 85313a7e), so a reviewer spawn carries the harness-rendered preamble
    /// and stamps its round on the plan frame.
    fn register_spawn_tool(
        &self,
        registry: &mut ToolRegistry,
        workflow: &Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle>,
        project_root: &Path,
        agent_id: Option<crate::runtime::AgentId>,
        sel: Option<&ToolSelection>,
    ) {
        if !group_allowed(sel, ToolGroup::Spawn) {
            // 2c.2 (G2): the selection excludes the Spawn group.
            return;
        }
        if let Some(spawner) = self.spawner.read().expect("spawner lock poisoned").clone() {
            let mut tool = match agent_id {
                Some(id) => SpawnAgentTool::new(spawner).with_parent(id),
                None => SpawnAgentTool::new(spawner),
            };
            // 2c.5 (§5.4 :377): the spawn param assembly carries the
            // parent's policy — subagents inherit it. The factory wires
            // the same ApproveViaApi it sets on the loops it builds; a
            // free parent's OverridePermissions is threaded by the 3a
            // Engine's own registration.
            tool = tool.with_permission(
                crate::agent::policy::PermissionPolicy::ApproveViaApi {
                    safety_mode: *self.safety_mode.read().expect("safety_mode lock poisoned"),
                },
            );
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
            // The review-round scope (backlog 85313a7e): a reviewer spawn then
            // gets the harness-rendered preamble appended to its task — the
            // verdict contract, the one-line constitution checks, the
            // `.coding/**` bookkeeping rule, and for round >= 2 the delta scope
            // (the delta since `<base>`, named via the reviewer's own
            // `git_read` ops, + the changed file set) — and stamps its round on
            // the plan frame once the spawn succeeds.
            tool = tool.with_review_scope(
                Arc::clone(workflow),
                project_root.to_path_buf(),
                Arc::new(crate::agent::review_scope::GitReviewScoper),
            );
            registry.register(Box::new(tool));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 2b.3: the production imports retired with the kernel decoupling; the
    // tests keep the concrete app types (cfg(test) — scan-excluded).
    use crate::app::workflow::Workflow;
    use tokio::sync::Mutex;

    /// The concrete app workflow behind a loop's neutral handle (2b.3): the
    /// tests read the rich surface through the same app→app downcast the IPC
    /// uses.
    fn concrete(
        handle: &Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle>,
    ) -> Arc<Mutex<Workflow>> {
        handle
            .as_any_arc()
            .downcast_ref::<Arc<Mutex<Workflow>>>()
            .expect("the loop's workflow handle wraps the concrete app workflow")
            .clone()
    }
    use crate::config::SafetyMode;
    use crate::memory::embedder::HashEmbedder;
    use crate::memory::MemoryStore;
    use crate::provider::{
        Capabilities, FinishReason, LlmEvent, Message, ProviderKind, ToolSchema,
    };
    use crate::app::tool::workflow::plan::CreatePlanTool;
    use crate::tool::Tool;
    use crate::app::skill::SkillLibrary;
    use crate::app::workflow::WorkflowState;
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

    /// G4 (2c.4, §5.9.4; acceptance row 28): the steering counters are
    /// factory-scoped — the process-wide `SteeringStats::shared()`
    /// singleton is deleted (any survivor fails to compile, the
    /// tripwire). Two factories must never share one counter set, and a
    /// fresh factory's set starts zeroed.
    #[test]
    fn two_factories_have_disjoint_steering_stats() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = make_factory(dir_a.path());
        let b = make_factory(dir_b.path());
        assert!(!Arc::ptr_eq(a.steering(), b.steering()));
        assert_eq!(
            b.steering().snapshot(),
            crate::agent::steering_stats::SteeringStats::new().snapshot(),
        );
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
        // Test hygiene (backlog fc1d57fe): the staleness log lives in the
        // temp dir — no test may write the real ~/.mnemo.
        let factory = AgentLoopFactory::new(
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
        .with_staleness_log(crate::index_staleness::StalenessLog::at(
            dir.join(".mnemo-staleness"),
        ));
        // 2b.3: the production shape wires the app policy factory — without
        // it a build's loop gets the UnwiredWorkflow handle and every
        // concrete-reading assert fails. The None wiring = the test base
        // (per-test wirings replace it — the plain-slot write, last wins).
        factory.set_policy_factory(Arc::new(
            crate::app::policy_factory::WorkflowPolicyFactory::new(
                None,
                None,
                None,
                std::path::PathBuf::new(),
                None,
            ),
        ));
        factory
    }

    /// Build the kernel-only factory (2c.2's tool-selection tests): the
    /// `make_factory` shape WITHOUT the app policy factory — the registry
    /// carries no app-owned tools (the free agent's shape; row 5's letter).
    fn make_free_factory(dir: &std::path::Path) -> AgentLoopFactory {
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
            None,
        )
        .with_staleness_log(crate::index_staleness::StalenessLog::at(
            dir.join(".mnemo-staleness"),
        ))
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
            let handle = concrete(&agent_a.workflow_handle());
            let wf_a = handle.lock().await;
            let handle = concrete(&agent_b.workflow_handle());
            let wf_b = handle.lock().await;
            assert_eq!(wf_a.state(), WorkflowState::Planning);
            assert_eq!(wf_b.state(), WorkflowState::Planning);
        }

        // Create a plan in agent A's workflow directly.
        {
            let handle = concrete(&agent_a.workflow_handle());
            let mut wf_a = handle.lock().await;
            wf_a.create_plan("A's plan", "goal", "ctx", vec!["step1".into()])
                .unwrap();
        }

        // Agent A is now Executing; agent B must still be Planning.
        {
            let handle = concrete(&agent_a.workflow_handle());
            let wf_a = handle.lock().await;
            let handle = concrete(&agent_b.workflow_handle());
            let wf_b = handle.lock().await;
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
            let handle = concrete(&agent_a.workflow_handle());
            let mut wf_a = handle.lock().await;
            wf_a.create_plan("Resume me", "goal", "ctx", vec!["a".into(), "b".into()])
                .unwrap();
            wf_a.complete_step(0).unwrap();
        }

        // Agent B is built from the same factory — it should load the plan
        // from disk and resume at step 1 (Executing, one step done).
        let agent_b = factory.build();
        let handle = concrete(&agent_b.workflow_handle());
        let wf_b = handle.lock().await;
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
        // 2c.3 (G3): the git pair is repo-gated — the repo marker.
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
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
    /// A spawner that records what it was asked to spawn (review round 1,
    /// LOW 3: the factory's reviewer wiring had no witness).
    struct RecordingSpawner {
        calls: std::sync::Mutex<Vec<(String, String, Option<String>)>>,
    }

    impl RecordingSpawner {
        fn new() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl AgentSpawner for RecordingSpawner {
        async fn spawn(
            &self,
            name: &str,
            task: &str,
            role: Option<String>,
            // §5.4 :377 — accepted for the trait; the reviewer-wiring
            // witness does not assert on it.
            _permission: crate::agent::policy::PermissionPolicy,
        ) -> Result<crate::runtime::AgentId, String> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_string(), task.to_string(), role));
            Ok(1)
        }
    }

    /// Backlog 85313a7e, review round 1 LOW 3: every behaviour test builds the
    /// reviewer tools with their builders directly, so DROPPING a registration
    /// call in the factory reverted the whole feature with a green suite. This
    /// drives the tools through the REGISTRY instead.
    #[tokio::test]
    async fn registered_reviewer_tools_carry_their_wiring() {
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let spawner = Arc::new(RecordingSpawner::new());
        factory.set_spawner(spawner.clone());
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );

        // The spawn path: a reviewer spawn through the REGISTERED tool must
        // carry the harness-rendered contract even though the task supplies
        // none of it — i.e. `with_review_scope` is wired at registration, and
        // the registered tool is bound to the factory's own workflow + project
        // root.
        let tool = registry.get("spawn_agent").expect("spawn_agent registered");
        let result = tool
            .execute(json!({
                "name": "reviewer",
                "task": "review the diff",
                "role": "reviewer"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        {
            let calls = spawner.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert!(
                calls[0].1.contains("## Reviewer contract"),
                "the registered spawn tool must render the harness contract: {}",
                calls[0].1
            );
            assert!(
                calls[0].1.contains("## Verdict: PASS"),
                "the contract must carry the verdict contract: {}",
                calls[0].1
            );
        }

        // The report path: its provenance stamp needs a real git repository to
        // witness (its behaviour is pinned by the `FakeScoper` tests in the
        // tool's own module), so the REGISTRATION is asserted structurally —
        // through the same construction point the registry uses.
        assert!(
            factory
                .build_write_review_report_tool(dir.path())
                .provenance_wired(),
            "the registered report tool must carry its provenance scoper"
        );
    }

    #[test]
    fn tools_array_stays_within_context_budget() {
        use crate::tool::ToolFilter;
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );

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
            let r2 = f2.build_registry(
                &crate::app::workflow::as_handle(wf2.clone()),
                None,
                None,
                f2.plans_dir(),
            );
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
            // 19_500 → 20_100 (2027-01-25): token-optimizer lever 3
            // (backlog e4a50d22) adds the always-advertised expand_result
            // schema (~+418, rides every filter); measures Planning at 19_659
            // chars. Ceiling = measured + headroom, deliberate raise.
            // 20_100 → 21_800 (2027-01-11): the probe-evidence gate (backlog
            // 5b232b8d) — create_plan's description gains the
            // findings-not-assumptions doctrine, its `context` property the
            // evidence rule + marker vocabulary, update_plan's `context` the
            // append-scoping note (~+1_300 together, all schema text that
            // rides Planning); measures Planning at 21_402 chars
            // (workspace-unified) on the final tree. The remaining ~+440 of
            // the delta over the 19_659 figure pinned at the 2027-01-25 pass
            // is schema drift from plans landed since. Ceiling = measured +
            // headroom, deliberate raise.
            (ToolFilter::Planning, 21_800),
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
            // 36_300 → 37_300 (2027-01-11): same cause as the Planning raise
            // above (expand_result, ~+418); measures Executing at 36_963
            // chars. Ceiling = measured + headroom, deliberate raise.
            // 37_300 → 37_900 (2026-09-27): file_edit and multi_edit gain the
            // `artifact_check` escape-hatch property (backlog 08d2125d) — the
            // property must stay DECLARED because strict providers validate
            // args against the schema, so the terse ~+280 chars cannot move to
            // the rejection message alone; measures Executing at 37_600 chars.
            // Ceiling = measured + headroom, deliberate raise.
            // 37_900 → 39_600 (2027-01-11): the probe-evidence gate (backlog
            // 5b232b8d) — the same create_plan description + `context` property
            // and update_plan `context` growth that raised Planning above
            // (~+1_300) rides Executing, on top of drift since the 37_600
            // figure pinned at the 2026-09-27 pass; measures Executing at
            // 39_150 chars (workspace-unified, 26 tools on the final tree).
            // Ceiling = measured + headroom, deliberate raise.
            (ToolFilter::Executing, 39_600),
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
            // 37_600 → 38_900 (2027-01-11): same cause as the Planning raise
            // above (expand_result, ~+418; PlanFrozen = Executing ∪ finish);
            // measures PlanFrozen at 38_249 chars. Ceiling = measured +
            // headroom, deliberate raise.
            // 38_900 → 40_900 (2027-01-11): the probe-evidence gate (backlog
            // 5b232b8d) — PlanFrozen advertises create_plan + update_plan, so
            // the same schema growth that raised Planning and Executing
            // (~+1_300) rides this filter too, on top of drift since the last
            // pass; measures PlanFrozen at 40_436 chars (workspace-unified,
            // 27 tools on the final tree). Ceiling = measured + headroom,
            // deliberate raise.
            (ToolFilter::PlanFrozen, 40_900),
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
            // 28_600 → 29_100 (2027-01-11): same cause as the Planning raise
            // above (expand_result, ~+418); measures ExecutingResearch at
            // 28_652 chars. Ceiling = measured + headroom, deliberate raise.
            // 29_100 → 30_900 (2027-01-11): the probe-evidence gate (backlog
            // 5b232b8d) — create_plan rides the research filters too, so the
            // same schema growth that raised Planning and Executing (~+1_300)
            // rides here, on top of drift since the last pass; measures
            // ExecutingResearch at 30_481 chars (workspace-unified, 22 tools
            // on the final tree). Ceiling = measured + headroom, deliberate
            // raise.
            (ToolFilter::ExecutingResearch, 30_900),
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
            // 31_600 → 32_700 (2027-01-11): same cause as the Planning raise
            // above (expand_result, ~+418); measures Reviewing at 32_227
            // chars. Ceiling = measured + headroom, deliberate raise.
            // 32_700 → 33_100 (2026-09-27): same cause as the Executing raise
            // above (the `artifact_check` property rides file_edit + multi_edit
            // schemas, shared with the Reviewer's advertised array); measures
            // Reviewing at 32_864 chars. Ceiling = measured + headroom,
            // deliberate raise.
            // 33_100 → 33_900 (2027-01-11): the probe-evidence gate (backlog
            // 5b232b8d) — Reviewing advertises update_plan, whose `context`
            // property gained the append-scoping note (~+150), on top of drift
            // since the 32_864 figure pinned at the 2026-09-27 pass; measures
            // Reviewing at 33_434 chars (workspace-unified, 25 tools on the
            // final tree). Ceiling = measured + headroom, deliberate raise.
            (ToolFilter::Reviewing, 33_900),
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
            // 19_500 → 20_100 (2027-01-11): same cause as the Planning raise
            // above (expand_result, ~+418; Complete carries the same read-only
            // set as Planning); measures Complete at 19_659 chars. Ceiling =
            // measured + headroom, deliberate raise.
            // 20_100 → 21_800 (2027-01-11): same cause as the Planning raise
            // above (backlog 5b232b8d — create_plan's description + `context`
            // property and update_plan's `context`, ~+1_300); the same tool
            // set as Planning, so it measures Complete at 21_402 chars, plus
            // the same drift since the 19_659 figure pinned at the 2027-02-05
            // pass. Ceiling = measured + headroom, deliberate raise.
            (ToolFilter::Complete, 21_800),
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
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );
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
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );

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
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );
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
        let registry_no_vision = factory_no_vision.build_registry(
            &crate::app::workflow::as_handle(workflow2.clone()),
            None,
            None,
            factory_no_vision.plans_dir(),
        );
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
            _permission: crate::agent::policy::PermissionPolicy,
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
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );
        assert!(
            registry.get("spawn_agent").is_none(),
            "spawn_agent should NOT be registered without a spawner"
        );

        // Once a spawner is wired in, every subsequent build gets the tool.
        factory.set_spawner(Arc::new(MockSpawner));
        let registry2 = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );
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
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );
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
        let registry2 = factory2.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory2.plans_dir(),
        );
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
    fn tool_choice_gate_flips_live_through_the_factory() {
        // The gate's flag is the SAME Arc the rewire path flips via
        // `set_tool_choice_enabled`, so a Settings toggle reaches
        // already-built tools with no registry rebuild (backlog e2c47d5f).
        let dir = tempdir().unwrap();
        let handle = ToolChoiceHandle {
            classifier: Arc::new(std::sync::RwLock::new(None)),
            enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            log_path: None,
        };
        let flag = Arc::clone(&handle.enabled);
        let factory = make_factory(dir.path()).with_tool_choice(handle);
        factory.set_tool_choice_enabled(true);
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
        factory.set_tool_choice_enabled(false);
        assert!(!flag.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn expected_tool_names_registered_when_fully_wired() {
        // Maint M5: a fully-wired factory (skills + memory + spawner) must
        // register the complete expected tool name set. This guards against a
        // registration being silently dropped when the grouped helpers are
        // edited.
        let dir = tempdir().unwrap();
        let factory = make_factory(dir.path());
        // 2b.3: the skill tools register app-side — the library flows to the
        // app factory's ctor (the kernel's skills surface retired).
        let skills = Arc::new(SkillLibrary::from_registry(
            dir.path().join(".coding/skills"),
            crate::app::skill::SkillRegistry::new(),
        ));
        factory.set_spawner(Arc::new(MockSpawner));
        // The app-owned tools ride the app policy factory (2b.2): wire the
        // app factory into the kernel factory (the production build() path)
        // and carry the backlog store on it — fully wired = the Ready path's
        // shape.
        let policy_factory = Arc::new(crate::app::policy_factory::WorkflowPolicyFactory::new(
            None,
            None,
            None,
            std::path::PathBuf::new(),
            Some(skills.clone()),
        ));
        let factory_dyn: Arc<dyn crate::agent::policy::AgentPolicyFactory> =
            policy_factory.clone();
        factory.set_policy_factory(factory_dyn);
        policy_factory.set_backlog(Arc::new(tokio::sync::Mutex::new(
            crate::app::backlog::BacklogStore::open(dir.path().join("backlog.jsonl")),
        )));
        // 2c.3 (G3): the git pair is repo-gated — this test pins the fully-
        // wired shape, which includes the git pair (the repo marker).
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );

        let expected = vec![
            // agent/file tools
            "read_files",
            // progressive disclosure (backlog e4a50d22 lever 3): serves back a
            // tool result that was archived for size
            "expand_result",
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
            // skill library tools (reload: every state; create: Executing-only in the base states, grantable via a skill allow-list)
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

    /// A minimal canned resolver (2c.2's list_models gating test): the
    /// factory wiring just needs the slot PRESENT so `list_models`
    /// registers; nothing resolves in these tests.
    struct StubResolver;

    #[async_trait::async_trait]
    impl crate::model_resolver::ModelResolver for StubResolver {
        fn resolve(
            &self,
            _ctx: crate::model_resolver::ModelContext,
        ) -> Option<crate::config::ModelRef> {
            Some(crate::config::ModelRef {
                endpoint: "stub".into(),
                model: "stub-model".into(),
                reasoning_effort: None,
            })
        }
        fn build_turn_provider(
            &self,
            _model: &crate::config::ModelRef,
            _fill_rate: f64,
        ) -> Option<(
            Arc<dyn crate::provider::LlmClient>,
            crate::agent::context::ContextManager,
        )> {
            None
        }
    }

    #[test]
    fn tool_selection_registers_only_read_group() {
        // 2c.2 (G2, row 26's kernel form): a selection of just Read must
        // register exactly the read tools and nothing else — no write, no
        // shell, no reviewer channel (Host), no loader (Mcp), no
        // memory/vision/spawn, and no app tools (no policy factory wired).
        let dir = tempdir().unwrap();
        let factory = make_free_factory(dir.path()).with_tool_selection(ToolSelection {
            include: vec![ToolGroup::Read],
            exclude: Vec::new(),
        });
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );
        for name in ["read_files", "expand_result", "search", "search_read"] {
            assert!(
                registry.get(name).is_some(),
                "expected read tool '{name}' registered"
            );
        }
        let names: Vec<String> = registry
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        assert_eq!(
            names.len(),
            4_usize,
            "the Read-only selection must register exactly the read tools; got: {names:?}"
        );
    }

    #[test]
    fn free_default_excludes_the_reviewer_channel() {
        // 2c.2 (G2, row 5): the free agent's constant surface = every kernel
        // group EXCEPT Host — the reviewer channel (write_review_report) is
        // the workflow's, so a free agent advertises no review tool. The
        // workflow (None) default stays full (the expected-names test above).
        let dir = tempdir().unwrap();
        let factory =
            make_free_factory(dir.path()).with_tool_selection(ToolSelection::free_default());
        factory.set_spawner(Arc::new(MockSpawner));
        assert!(
            !registry_contains(&factory, &dir, "write_review_report"),
            "no review tool in the free surface"
        );
        for name in [
            "read_files",
            "file_edit",
            "file_write",
            "shell",
            "search",
            "web_fetch",
            "load_tools",
            "memory_write",
            "memory_search",
            "spawn_agent",
        ] {
            assert!(
                registry_contains(&factory, &dir, name),
                "expected '{name}' in the free surface"
            );
        }
        let count = registry_len(&factory, &dir);
        // The browser family is feature-gated (10 offscreen + 6 on-screen);
        // the git pair is repo-gated (2c.3) — the tempdir is not a repo.
        let expected = if cfg!(feature = "browser") { 42 } else { 26 };
        assert_eq!(
            count,
            expected,
            "the free surface = the kernel tools minus the review tool and the app tools and the (unrequired) git pair"
        );
    }

    /// Build a registry through the factory's production path — the shared
    /// harness of the tool-selection tests.
    fn registry_for_factory(factory: &AgentLoopFactory, dir: &tempfile::TempDir) -> ToolRegistry {
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        )
    }

    fn registry_contains(
        factory: &AgentLoopFactory,
        dir: &tempfile::TempDir,
        name: &str,
    ) -> bool {
        registry_for_factory(factory, dir).get(name).is_some()
    }

    fn registry_len(factory: &AgentLoopFactory, dir: &tempfile::TempDir) -> usize {
        registry_for_factory(factory, dir).iter().count()
    }

    #[test]
    fn list_models_and_spawn_require_the_spawn_group() {
        // 2c.2 (G2, §5.9.2 :641): list_models registers only with the Spawn
        // group; spawn_agent likewise rides its group gate.
        let dir = tempdir().unwrap();
        let factory = make_free_factory(dir.path())
            .with_model_resolver(Arc::new(StubResolver) as Arc<dyn crate::model_resolver::ModelResolver>)
            .with_tool_selection(ToolSelection {
                include: vec![ToolGroup::Spawn],
                exclude: Vec::new(),
            });
        factory.set_spawner(Arc::new(MockSpawner));
        let registry = registry_for_factory(&factory, &dir);
        assert!(registry.get("spawn_agent").is_some(), "spawn with the Spawn group");
        assert!(
            registry.get("list_models").is_some(),
            "list_models registers with the Spawn group + a resolver"
        );
        // And WITHOUT the group — even with both wires in — absent:
        let dir2 = tempdir().unwrap();
        let factory2 = make_free_factory(dir2.path())
            .with_model_resolver(Arc::new(StubResolver) as Arc<dyn crate::model_resolver::ModelResolver>)
            .with_tool_selection(ToolSelection {
                include: vec![ToolGroup::Read],
                exclude: Vec::new(),
            });
        factory2.set_spawner(Arc::new(MockSpawner));
        let registry2 = registry_for_factory(&factory2, &dir2);
        assert!(
            registry2.get("list_models").is_none(),
            "list_models must not register without the Spawn group"
        );
        assert!(
            registry2.get("spawn_agent").is_none(),
            "spawn_agent must not register without the Spawn group"
        );
    }

    #[test]
    fn tool_selection_exclude_carves_named_tools() {
        // 2c.2 (G2): the exclude leg of selects() — a named carve-out skips
        // just that tool inside an included group.
        let dir = tempdir().unwrap();
        let factory =
            make_free_factory(dir.path()).with_tool_selection(ToolSelection {
                include: ToolSelection::free_default().include,
                exclude: vec!["memory_search".to_string()],
            });
        factory.set_spawner(Arc::new(MockSpawner));
        let registry = registry_for_factory(&factory, &dir);
        assert!(
            registry.get("memory_search").is_none(),
            "memory_search must be carved out"
        );
        assert!(
            registry.get("memory_write").is_some(),
            "the rest of the Memory group stays"
        );
        let count = registry.iter().count();
        // The browser family is feature-gated (10 offscreen + 6 on-screen);
        // the git pair is repo-gated (2c.3) — the tempdir is not a repo.
        let expected = if cfg!(feature = "browser") { 41 } else { 25 };
        assert_eq!(
            count,
            expected,
            "free surface minus the carved tool and the (unrequired) git pair"
        );
    }

    #[test]
    fn git_tools_register_only_when_root_is_a_repo() {
        // 2c.3 (G3, row 27's kernel form): on a directory that is NOT a git
        // repo the Git group does not register (no git/git_read tools), and
        // the kernel must not create .git/.gitignore artifacts there. With a
        // repo marker present (the gate probes `.git` existence — a real
        // repo's dir OR a linked-worktree's plain file), both tools reappear.
        let dir = tempdir().unwrap();
        let factory =
            make_free_factory(dir.path()).with_tool_selection(ToolSelection::free_default());
        factory.set_spawner(Arc::new(MockSpawner));
        let registry = registry_for_factory(&factory, &dir);
        assert!(
            registry.get("git").is_none(),
            "no git tool on a non-repo root"
        );
        assert!(
            registry.get("git_read").is_none(),
            "no git_read tool on a non-repo root"
        );
        assert!(!dir.path().join(".git").exists());
        assert!(!dir.path().join(".gitignore").exists());
        // A repo marker flips the gate (the existence-probe contract; the
        // real-repo shape is covered by the integration git fixtures).
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let registry2 = registry_for_factory(&factory, &dir);
        assert!(registry2.get("git").is_some(), "git registers on a repo root");
        assert!(registry2.get("git_read").is_some());
    }

    #[test]
    fn skills_get_the_whole_read_surface_at_the_registry_level() {
        // backlog 834ec126: the Skill state grants the full read-only surface
        // wholesale (Agent + AutoRun). Audit that classification against the
        // REAL wired registry: every Agent+AutoRun tool must be reachable
        // inside a skill with an EMPTY allow-list, and every Agent+NeedsApproval
        // tool must stay invisible there — so a newly added read tool is
        // granted automatically, and a mutating tool mis-declared AutoRun fails
        // loudly instead of leaking into every skill.
        use crate::tool::{SafetyLevel, ToolCategory, ToolFilter};

        let dir = tempdir().unwrap();
        // 2c.3 (G3): the read surface includes the repo-gated git_read — the
        // repo marker sits the build up.
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let factory = make_factory(dir.path());
        let skills = Arc::new(SkillLibrary::from_registry(
            dir.path().join(".coding/skills"),
            crate::app::skill::SkillRegistry::new(),
        ));
        factory.set_spawner(Arc::new(MockSpawner));
        // 2b.2: the backlog trio registers via the app policy factory (D5) —
        // wire it the way production does before asserting the filter reads.
        let policy_factory2 = Arc::new(crate::app::policy_factory::WorkflowPolicyFactory::new(
            None,
            None,
            None,
            std::path::PathBuf::new(),
            Some(skills),
        ));
        let factory_dyn2: Arc<dyn crate::agent::policy::AgentPolicyFactory> =
            policy_factory2.clone();
        factory.set_policy_factory(factory_dyn2);
        policy_factory2.set_backlog(Arc::new(tokio::sync::Mutex::new(
            crate::app::backlog::BacklogStore::open(dir.path().join("backlog.jsonl")),
        )));
        let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );

        let skill_filter = ToolFilter::Skill(vec![]);
        let mut reads = Vec::new();
        for tool in registry.iter() {
            let name = tool.name().to_string();
            let (category, safety) = (tool.category(), tool.safety());
            // write_review_report is Agent+AutoRun but is NOT a read tool: the
            // guard at the top of `ToolFilter::allows` keeps it reviewer-only.
            if name == "write_review_report" {
                assert!(
                    !skill_filter.allows(category, safety, &name),
                    "write_review_report stays reviewer-only, whatever a skill lists"
                );
                continue;
            }
            // A TRUSTED MCP tool is AutoRun for APPROVAL only — excluded from
            // the read grant by name, exactly as in the base-state arms
            // (trust ≠ state visibility), so it can never ride into a skill.
            if name.starts_with("mcp__") {
                assert!(
                    !skill_filter.allows(category, safety, &name),
                    "{name}: per-server trust must never widen a skill's surface"
                );
                continue;
            }
            if category == ToolCategory::Agent && safety == SafetyLevel::AutoRun {
                assert!(
                    skill_filter.allows(category, safety, &name),
                    "{name} is read-only (Agent + AutoRun) — a skill must reach it without naming it"
                );
                reads.push(name);
            } else if category == ToolCategory::Agent && safety == SafetyLevel::NeedsApproval {
                assert!(
                    !skill_filter.allows(category, safety, &name),
                    "{name} mutates (NeedsApproval) — it must stay behind the skill allow-list"
                );
            }
        }
        // The Agent+AutoRun set IS the documented read surface: assert it
        // EXACTLY, so a newly added read tool must be classified here
        // consciously and a mutating tool mis-declared AutoRun fails loudly
        // (the factory registry is deterministic in this harness).
        reads.sort();
        assert_eq!(
            reads,
            vec![
                "expand_result",
                "git_read",
                "load_tools",
                "read_files",
                "search",
                "search_read",
                "web_fetch",
            ],
            "the Agent+AutoRun set IS the read surface a skill gets — classify a new tool here \
             consciously"
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
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );
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
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            None,
            None,
            factory.plans_dir(),
        );

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
        // Simulate sub-agent construction (as IpcSpawner does for parent_id.is_some())
        // — the spawn path's compound stamp (2b.3: the kernel rides the
        // neutral handle; the plan-mutation deny + the role state ride
        // app-side under one call).
        agent.set_plan_mutations_allowed(false);
        agent.workflow_handle().stamp_subagent_invariants().await;
        assert!(!agent.plan_mutations_allowed());

        // The create_plan tool itself now denies (defense in depth).
        let tool = CreatePlanTool::new(concrete(&agent.workflow_handle()));
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
        let handle = concrete(&agent.workflow_handle());
        let wf_guard = handle.lock().await;
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
        let handle = concrete(&agent.workflow_handle());
        let wf_guard = handle.lock().await;
        assert_eq!(wf_guard.plans_dir(), plans_dir);
        drop(wf_guard);

        // The registry's file tools bind to the OVERRIDE root: a write
        // lands in the worktree, never in the factory's project dir.
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(plans_dir.clone())));
        let registry = factory.build_registry(
            &crate::app::workflow::as_handle(workflow.clone()),
            Some(42),
            Some(&spec),
            &plans_dir,
        );
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
            let handle = concrete(&agent_a.workflow_handle());
            let mut wf = handle.lock().await;
            wf.create_plan("A's plan", "goal", "ctx", vec!["step1".into()])
                .unwrap();
        }

        // Agent A is Executing; agent B must still be Planning (no plan).
        {
            let handle = concrete(&agent_a.workflow_handle());
            let wf_a = handle.lock().await;
            let handle = concrete(&agent_b.workflow_handle());
            let wf_b = handle.lock().await;
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
            let handle = concrete(&child.workflow_handle());
            let wf = handle.lock().await;
            assert_eq!(
                wf.state(),
                WorkflowState::Executing,
                "load_latest derives the main plan's state (the mirror load)"
            );
        }
        // …then the spawn path's parented stamps replace it (spawn.rs) —
        // 2b.3: the compound stamp rides the neutral handle.
        child.workflow_handle().stamp_subagent_invariants().await;
        {
            let handle = concrete(&child.workflow_handle());
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
            !child.workflow_handle().expects_progress(false).await,
            "a Subagent-state loop never expects progress (auto-continue parks)"
        );
    }
}
