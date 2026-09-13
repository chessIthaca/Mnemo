// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Shared IPC state — held by Tauri as managed state.
//!
//! This bundles everything the commands + event forwarder need access to:
//! the agent manager, the per-agent loops, the project, the config, and the
//! pending-approvals map.
//!
//! **Per-agent workflow:** each spawned agent owns its own `AgentLoop` (built
//! by the [`AgentLoopFactory`](mnemo::agent::factory::AgentLoopFactory)),
//! which in turn owns its own `Workflow`. The `agent_loops` map lets the IPC
//! layer reach a specific agent's loop (and thus its plan) by id — used by
//! `get_workflow_state(agent_id)`. There is no longer a single shared
//! `Workflow`; multi-agent is safe for concurrent implementation.

use std::sync::{Arc, RwLock};

use tokio::sync::Mutex;

use mnemo::agent::factory::AgentLoopFactory;
use mnemo::agent::AgentLoop;
use mnemo::config::{Config, SafetyMode};
use mnemo::memory::MemoryStore;
use mnemo::project::Project;
use mnemo::runtime::{AgentId, AgentManager};
use mnemo::safety_rules::SafetyRules;
use mnemo::tool::agent::sandbox::Sandbox;

use crate::ipc::approval::PendingApprovals;
use crate::ipc::questions::PendingQuestions;
use mnemo::backlog::BacklogStore;

/// The shared state accessible to all Tauri commands.
///
/// Grouped into nested contexts so each command module reaches only the
/// subsystems it needs (Maint M4): [`AgentRuntimeContext`] (manager + loops +
/// factory + safety), [`ProjectContext`] (project + config + sandbox), and
/// [`BacklogContext`] (backlog + auto-feed + run-all).
///
/// **Lock-ordering invariant:** when both `manager` and `agent_loops` must be
/// held, always acquire `manager` first, then `agent_loops` — or (preferred)
/// snapshot under one, drop it, then read under the other (see `list_agents`).
/// Never acquire `agent_loops` then `manager` — that ordering is not used
/// anywhere today and would risk a deadlock if a future command did so while
/// another task held `manager` then waited on `agent_loops`.
pub struct IpcState {
    /// The agent runtime: manager, per-agent loops, factory, safety.
    pub runtime: AgentRuntimeContext,
    /// The project: project handle, config, sandbox.
    pub project: ProjectContext,
    /// Pending approval requests (holds oneshot senders).
    pub approvals: Arc<PendingApprovals>,
    /// Pending `ask_user` questions (holds oneshot senders). Twin of
    /// [`approvals`](Self::approvals).
    pub questions: Arc<PendingQuestions>,
    /// The backlog subsystem: store + auto-feed + run-all state.
    pub backlog: BacklogContext,
    /// The shared LLM request/response trace log (the right-panel "Trace"
    /// tab). The same `Arc` is wired into every provider (default provider,
    /// resolver-built per-context providers, `set_model`/Settings rewires) so
    /// all `/chat/completions` traffic lands in one log. Always available —
    /// even when the brain failed to build (a fresh log is created on the
    /// fallback path, like the browser manager).
    pub trace: Arc<mnemo::provider::trace::LlmRequestLog>,
    /// The native child WebView2 for the Browser tab (replaces the iframe).
    /// Holds the webview handle + rect + overlay-depth + tab-visible flag
    /// behind one async mutex, plus a deferred-rect slot that self-heals
    /// `Busy`-dropped rect updates (review F3, 2026-08-18). Always available
    /// — created empty in all three brain branches (Ready/NeedsProject/Err),
    /// mirroring `browser`.
    pub browser_webview: Arc<crate::ipc::browser_webview::BrowserWebviewShared>,
    /// The agent-chat (React) child WebView2 — the app's main UI, created as an
    /// `add_child` of the bare "main" window (path (a): bare WindowBuilder +
    /// two ordered add_child webviews). Held so the window-resize handler can
    /// keep it filling the window, and so the exit handler can drop the handle
    /// (no HWND leak). `std::sync::Mutex` (not tokio) because the resize handler
    /// is sync. Always `Some` after setup; `None` only transiently at exit.
    pub agent_chat_webview: Arc<std::sync::Mutex<Option<tauri::webview::Webview>>>,
    /// The hang watchdog — writes `hang-*.txt` evidence reports when the main
    /// thread stalls (see `crate::watchdog`). Started in every setup branch
    /// (Ready / NeedsProject / Err) so a hang in any UI state is captured;
    /// `None` only in tests that construct `IpcState` directly.
    pub watchdog: Option<Arc<crate::watchdog::Watchdog>>,
}

/// The per-agent loop map: `Arc<tokio::Mutex<HashMap<AgentId, Arc<AgentLoop>>>>`.
/// Shared across the IPC layer (state, spawn, events). A type alias so the
/// 8+ spellings of this type don't drift.
pub(crate) type AgentLoopMap =
    Arc<tokio::sync::Mutex<std::collections::HashMap<AgentId, Arc<AgentLoop>>>>;

/// The agent-runtime slice of [`IpcState`].
pub struct AgentRuntimeContext {
    /// The agent manager (spawned agents, fan-in).
    pub manager: Arc<Mutex<AgentManager>>,
    /// The per-agent loops, keyed by agent id. Each agent owns its own
    /// `AgentLoop` (and thus its own `Workflow`), so `get_workflow_state`
    /// can read a specific agent's plan. Entries are inserted by
    /// `spawn_agent` and removed by the event forwarder on `Exited`.
    pub agent_loops: AgentLoopMap,
    /// The last `ContextUsage` per agent — `(used, max)` tokens, keyed by
    /// agent id, updated by the event forwarder on every usage event and
    /// removed on `Exited`. The app's only context-fill source: the run-all
    /// between-items auto-compact gate (backlog ffd4bac3) reads the main
    /// agent's entry to decide whether the post-item context is at/above
    /// the effective fill-rate threshold before compacting.
    pub context_usage: Arc<Mutex<std::collections::HashMap<AgentId, (u64, u64)>>>,
    /// The factory that builds per-agent loops. `None` when the brain failed
    /// to build at startup (the frontend shows the startup-error screen).
    pub factory: Option<Arc<AgentLoopFactory>>,
    /// Concrete memory store (for runtime embedder swaps). `None` when the
    /// brain failed to build or memory opened in-memory-only as a trait object
    /// without a concrete handle.
    pub memory_store: Option<Arc<MemoryStore>>,
    /// The runtime safety mode — shared with every agent loop (via the
    /// factory) so the UI can toggle auto-approve at runtime. Read on every
    /// tool call.
    pub safety_mode: Arc<RwLock<SafetyMode>>,
    /// The safety-rules store — regex-based auto-approve rules backed by
    /// `.coding/safety.toml`. Shared with the agent loops (so matching calls
    /// skip the approval prompt) and editable from the Safety tab. `None` when
    /// the brain failed to build at startup.
    pub safety_rules: Option<Arc<SafetyRules>>,
    /// The per-context model resolver (shared with the factory). The IPC layer
    /// pushes reloaded config into it after a Settings save so `[models]`
    /// overrides take effect on the next turn. `None` when the brain failed to
    /// build at startup.
    pub model_resolver: Option<Arc<mnemo::model_resolver::ConfigModelResolver>>,
    /// The shared headless debug browser (the same `Arc` the agent tools use,
    /// via the factory) so the Browser tab and the agent drive one browser.
    /// Always available — even when the brain failed to build (a fresh
    /// manager is created on the fallback path).
    pub browser: Arc<mnemo::browser::BrowserManager>,
    /// If the brain failed to build at startup, this holds the error message.
    /// The frontend checks this on mount and shows an error screen instead of
    /// the normal UI.
    pub startup_error: Option<String>,
    /// True when no project was resolved at startup (the cwd is not inside a
    /// project, no `--project` flag, no pending-project marker). The frontend
    /// shows the project picker instead of the normal UI. The window is open
    /// and the config is loaded (so the picker can list registered projects),
    /// but the agent can't run until a project is chosen.
    pub needs_project: bool,
    /// The live status of the memory embedder (Ready / Checking / Fallback /
    /// Pulling / Failed). Probed at startup; updated by the probe task and
    /// emitted to the frontend via the `embedder://status` event so the UI can
    /// show a banner when semantic recall has degraded to keyword-only.
    pub embedder_status: Arc<RwLock<mnemo::memory::embedder::EmbedderStatus>>,
    /// The same-project instance conflict resolved at startup (main.rs):
    /// `Some` when another live mnemo instance already holds this project —
    /// the frontend warns before opening it (2027-01-13). Computed once, so
    /// the startup snapshot is stable for the whole session.
    pub instance_conflict: Option<crate::ipc::startup::InstanceConflict>,
}

/// The project slice of [`IpcState`].
pub struct ProjectContext {
    /// The project.
    pub root: Arc<Mutex<Project>>,
    /// The global config.
    pub config: Arc<Mutex<Config>>,
    /// The path sandbox — confines file operations to the project root.
    pub sandbox: Arc<Sandbox>,
}

/// A recorded user intervention (steer or interrupt) on the main agent.
#[derive(Debug, Clone)]
pub struct UserIntervention {
    /// The backlog item the intervention applies to, when it was captured at
    /// halt time (`halt_run_all` fills this from the run-all's in-flight item
    /// — the latch is the reliable capture: the interrupt path never halts,
    /// and the run state may be gone by consumption time). `None` resolves
    /// via `run_all.current_item` / `single_in_flight` at consumption time
    /// instead.
    pub item_id: Option<String>,
    /// Why the intervention happened — used verbatim in the intervention
    /// note (e.g. "steered by the user", "interrupted by the user").
    pub reason: String,
}

/// The backlog slice of [`IpcState`].
pub struct BacklogContext {
    /// The persistent backlog of prompts waiting for dispatch to the main
    /// agent, backed by `.coding/backlog.jsonl`. Always available — even when
    /// the brain failed to build — so the list stays viewable.
    pub store: Arc<Mutex<BacklogStore>>,
    /// Whether auto-feed is on: when the main agent goes idle and this is
    /// true, the top pending backlog item is dispatched automatically.
    /// Session-only (never persisted) and always starts `false` — the user
    /// opts in per session. Run-All turns it off (they never fight).
    pub auto_feed: Arc<std::sync::atomic::AtomicBool>,
    /// Whether PARALLEL run-all is on (plan ffd7a86f): when the user hits
    /// Run-All with this checked, items beyond the first dispatch
    /// concurrently to spawned worktree agents (concurrency 3). Session-only
    /// (never persisted) and always starts `false` — the user opts in per
    /// session, exactly like auto-feed. Gates run-all concurrency ONLY;
    /// auto-feed itself stays sequential (the single-item conveyor).
    pub parallel_run_all: Arc<std::sync::atomic::AtomicBool>,
    /// State of the Run-All loop (`None` when no run is active). Holds the
    /// stop flag the loop checks between items.
    pub run_all: Arc<Mutex<Option<RunAllState>>>,
    /// The last run's completion note (`None` until a run whose agents wrote
    /// knowledge files ends; cleared when the next run starts). Surfaced in
    /// the backlog header next to the run controls (memory review
    /// 2026-09-08, suggestion 3).
    pub run_completion_note: Arc<std::sync::Mutex<Option<String>>>,
    /// The backlog item currently dispatched to the main agent on the
    /// single-dispatch / auto-feed path (`None` = none). Run-All tracks its
    /// own `current_item`; this covers manual dispatch + auto-feed so the
    /// item can be resolved (Done/Failed) when its turn completes instead of
    /// sticking at InFlight forever.
    pub single_in_flight: Arc<std::sync::Mutex<Option<String>>>,
    /// Set when the user steers or interrupts the MAIN agent while a backlog
    /// item is in flight. Consumed at the top of the turn-resolution path
    /// (`run_all::on_main_turn_resolved`), which then disposes of the
    /// affected item non-terminally (a run-all item is kept InFlight with
    /// its run stopped, backlog b83e891f; a single-dispatch item is kept
    /// InFlight with its pointer restored, plan cace17a6) instead of
    /// marking it Failed/CantResolve — a user
    /// intervention is not a task failure; it is the user taking the wheel.
    /// Consumed once, then cleared.
    pub user_intervention: Arc<std::sync::Mutex<Option<UserIntervention>>>,
    /// Run-All auto-compact signal: a counter the event forwarder increments
    /// when the MAIN agent's compaction completes (`Compacted` event) or
    /// fails (the `Error` paired with its `CompactStarted`). The spawned
    /// between-items auto-compact task subscribes before sending
    /// `AgentCommand::Compact` and waits for the counter to move past its
    /// recorded value — see `run_all::compact_then_dispatch_next`.
    pub compact_signal: Arc<tokio::sync::watch::Sender<u64>>,
    /// `true` while the Run-All auto-compact is compacting the main agent's
    /// context between items — surfaces the "compacting…" state in the
    /// run-all progress line. Cleared before the next item is dispatched.
    pub compacting: Arc<std::sync::atomic::AtomicBool>,
    /// The Run-All generation: bumped every time a run is armed. The
    /// heartbeat captures the generation at spawn and exits when it
    /// changes — a heartbeat from a previous run cannot survive into a
    /// successor armed within one heartbeat period (review 2026-09-09
    /// LOW-4; `backlog_run_all` refuses concurrent starts, so a successor
    /// only starts after the previous run ended — but the None gap can be
    /// shorter than the heartbeat's sleep phase).
    pub run_generation: Arc<std::sync::atomic::AtomicU64>,
}

/// Live state for an in-progress Run-All loop.
pub struct RunAllState {
    /// Set to true to ask the loop to stop after the current item.
    pub stop: std::sync::atomic::AtomicBool,
    /// The id of the backlog item currently in flight, if any.
    pub current_item: std::sync::Mutex<Option<String>>,
    /// How many items have completed (done/failed/cant-resolve) this run.
    pub done: std::sync::atomic::AtomicU64,
    /// Total pending items when the run started.
    pub total: std::sync::atomic::AtomicU64,
    /// The run's dispatch concurrency (plan ffd7a86f): 1 = today's
    /// sequential behavior (item 1 on the main agent, nothing spawned);
    /// N > 1 = items beyond the first dispatch concurrently to spawned
    /// worktree agents. Clamped to 1..=8 at the command boundary.
    pub concurrency: usize,
    /// The concurrently dispatched items (plan ffd7a86f): one entry per
    /// spawned worktree agent beyond the main agent's item. Empty when
    /// concurrency is 1 — the sequential path never touches this.
    pub spawned: std::sync::Mutex<Vec<SpawnedRun>>,
    /// The process-global knowledge-write counter snapshot taken when the
    /// run was armed — `end_run` diffs it against the live counter to
    /// surface "wrote N knowledge record(s) during this run" (memory review
    /// 2026-09-08, suggestion 3).
    pub knowledge_writes_at_start: std::sync::atomic::AtomicU64,
}

/// One concurrently dispatched run-all item (plan ffd7a86f): the spawned
/// parentless agent working it, its git worktree, and its branch. The
/// per-agent turn resolution (`on_spawned_turn_resolved`) looks its item
/// up by agent id here.
#[derive(Clone)]
pub struct SpawnedRun {
    /// The spawned parentless agent working this item.
    pub agent_id: AgentId,
    /// The backlog item id.
    pub item_id: String,
    /// The item's git worktree root (the agent's sandbox/project root).
    pub worktree: std::path::PathBuf,
    /// The item's branch (`wt/runall-<item8>`), landed app-side on Done.
    pub branch: String,
}

impl IpcState {
    /// The agent factory, or an error if the brain failed to start.
    pub fn factory(&self) -> Result<&Arc<AgentLoopFactory>, crate::ipc::error::IpcError> {
        self.runtime
            .factory
            .as_ref()
            .ok_or_else(|| "agent factory unavailable (brain failed to start)".into())
    }

    /// The safety-rules store, or an error if the brain failed to start.
    pub fn safety_rules(&self) -> Result<&Arc<SafetyRules>, crate::ipc::error::IpcError> {
        self.runtime
            .safety_rules
            .as_ref()
            .ok_or_else(|| "safety rules unavailable (brain failed to start)".into())
    }

    /// Set the runtime safety mode + re-evaluate pending approvals against it.
    /// Returns the count of pending approvals auto-resolved by the new mode
    /// (except core ops like git merge/push, which always stay gated). Returns
    /// an error if the safety-mode lock is poisoned.
    pub fn set_safety(&self, mode: SafetyMode) -> Result<usize, crate::ipc::error::IpcError> {
        *self
            .runtime
            .safety_mode
            .write()
            .map_err(|e| format!("safety_mode lock poisoned: {e}"))? = mode;
        let resolved = self.approvals.re_evaluate(mode, &self.project.sandbox);
        if resolved > 0 {
            eprintln!("safety: mode change auto-resolved {resolved} pending approval(s)");
        }
        Ok(resolved)
    }

    /// The live embedder status (cloned).
    pub fn embedder_status(
        &self,
    ) -> Result<mnemo::memory::embedder::EmbedderStatus, crate::ipc::error::IpcError> {
        Ok(self
            .runtime
            .embedder_status
            .read()
            .map_err(|e| format!("embedder status lock poisoned: {e}"))?
            .clone())
    }
}
