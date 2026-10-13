// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Agent-spawn Tauri command + the shared spawn path.
//!
//! `spawn_agent_shared` is the single spawn implementation used by the UI
//! button (`spawn_agent`), the agent tool (`IpcSpawner`), and main-agent
//! startup (main.rs). Every spawned agent gets its own loop + workflow,
//! registers with the manager, and is tracked in the per-agent loop map.

use std::sync::Arc;

use tauri::State;

use mnemo::agent::factory::AgentLoopFactory;
use mnemo::runtime::channels::AgentId;
use mnemo::runtime::{AgentManager, AgentSpawner, ParentAwareSpawner};

use crate::ipc::agent::AgentInfo;
use crate::ipc::error::IpcError;
use crate::ipc::events::emit_prompt_dispatched;
pub(crate) use mnemo::host::runtime::{spawn_agent_shared, WorktreeBinding};
use crate::ipc::state::IpcState;

/// Spawn a new agent with the given name.
///
/// Allocates a new agent id, creates a command channel + `AgentTask` (built
/// via the `AgentLoopFactory` so the new agent gets its own workflow + tool
/// registry while sharing the provider, memory store, sandbox, and safety
/// rules), spawns the task, and registers the handle. The new agent's loop is
/// tracked in the `agent_loops` map so `get_workflow_state(agent_id)` can read
/// its plan. Returns the new agent's id + name.
#[tauri::command]
pub async fn spawn_agent(state: State<'_, IpcState>, name: String) -> Result<AgentInfo, IpcError> {
    let factory = state
        .runtime
        .factory
        .clone()
        .ok_or_else(|| IpcError::msg("agent factory unavailable (brain failed to start)"))?;

    let (agent_id, name) = spawn_agent_shared(
        factory,
        state.runtime.manager.clone(),
        state.runtime.agent_loops.clone(),
        name,
        None,
        None, // no parent — a UI-button spawn has no owning agent
        None, // no forced model — UI spawns use the default
        None, // no role — UI spawns are unrestricted
        true, // own_plans_dir — UI spawns get .coding/plans/agents/<id>/
        None, // no worktree binding — a main-tree agent
        // 3a.5: keep the factory build's default policy (pre-3a.5 caller)
        None,
        // 3a.6 (G5): the IPC path never resumes from a transcript
        None,
        // 3a.7 (G7): the IPC path sets no session limits
        None,
        // 3c (G2): the IPC path registers no host tools
        vec![],
    )
    .await?;

    // Read the model + serving endpoint + effective effort from the
    // freshly-tracked loop (the agent was just built from the factory's
    // current provider, so these are its starting values; the effort is the
    // factory-stamped default — no turn has run yet, backlog 51dab4da).
    let (model, provider, reasoning_effort) = state
        .runtime
        .agent_loops
        .lock()
        .await
        .get(&agent_id)
        .map(|l| {
            (
                Some(l.provider().model().to_string()),
                l.effective_provider_name(),
                l.resolved_effort().or_else(|| l.default_display_effort()),
            )
        })
        .unwrap_or((None, None, None));

    Ok(AgentInfo {
        id: agent_id,
        name,
        running: false,
        parent_id: None,
        model,
        provider,
        reasoning_effort,
    })
}

/// Spawn a parallel run-all worktree agent (plan ffd7a86f): a PARENTLESS
/// agent bound to the item's git worktree — its file/shell/search tools,
/// git tools, and code graph all operate on the worktree root, and its
/// plans dir lives inside the worktree (plan bookkeeping lands on the
/// item's own branch). The code graph is built fresh over the worktree
/// (one index pass) — the main tree's graph would answer for the wrong
/// files. The worktree's DB is seeded from the main tree's DB first
/// (2026-09-08 performance LOW-2 lever b): the index pass re-checks every
/// file's content hash against the worktree's actual bytes, so unchanged
/// files skip the parse and only branch-diff files re-parse — the graph
/// still answers for the worktree's files, never the main tree's.
pub(crate) async fn spawn_run_all_agent(
    factory: Arc<AgentLoopFactory>,
    manager: Arc<tokio::sync::Mutex<AgentManager>>,
    agent_loops: crate::ipc::state::AgentLoopMap,
    worktree: std::path::PathBuf,
    name: String,
    initial_prompt: Option<String>,
) -> Result<(AgentId, String), String> {
    // Build the worktree's code graph off-thread: a fresh worktree has no
    // codegraph.db (gitignored), so this is a cold full-index pass —
    // potentially seconds on a large tree. Seed it from the main tree's DB
    // first (2026-09-08 performance LOW-2 lever b): the index pass's
    // content-hash check turns the cold parse into a read+hash sweep that
    // re-parses only branch-diff files. Best-effort — a seed failure (no
    // main graph, missing DB, lock) falls through to the cold pass, never
    // worse than today. A graph-open failure degrades to `None` (the
    // graph_* tools are omitted for that agent — same as a main-tree open
    // failure), never blocks the dispatch.
    let codegraph = {
        let wt = worktree.clone();
        // The main tree's DB path, derived via `Project` — the single
        // source of truth for the path convention (the same one the live
        // main graph was opened with). `None` when the main graph is
        // disabled or failed to open — no seeding, cold as before.
        let main_db = factory.codegraph_handle().map(|g| {
            mnemo::app::project::Project::from_root(g.root().to_path_buf()).codegraph_db
        });
        tokio::task::spawn_blocking(move || {
            let db = wt.join(".coding").join("codegraph.db");
            let seeded = main_db
                .as_deref()
                .map(|src| match mnemo::codegraph::snapshot_db(src, &db) {
                    Ok(()) => true,
                    Err(e) => {
                        // Best-effort: fall through to the cold pass, but
                        // leave a trace — a lane that silently degrades to
                        // cold (locked DB, missing file, path drift) is
                        // otherwise undiagnosable in the field (review
                        // LOW-5, 2026-09-08).
                        eprintln!("codegraph: worktree seed failed (cold pass): {e}");
                        false
                    }
                })
                .unwrap_or(false);
            mnemo::codegraph::CodeGraph::open(wt.clone(), &db)
                .map(|g| {
                    // One index pass (seeded: read+hash sweep + branch-diff
                    // re-parses; cold: full parse); a failure leaves the
                    // (empty) graph live — per-call tool errors, not a
                    // teardown.
                    match if seeded { g.index_seeded(None) } else { g.index(None) } {
                        Ok(s) => eprintln!(
                            "codegraph: worktree graph {} — scanned {} files, \
                             re-parsed {}, pruned {}, {} symbols in {}ms",
                            if seeded { "seeded" } else { "cold" },
                            s.files_scanned,
                            s.files_reindexed,
                            s.files_pruned,
                            s.symbols,
                            s.elapsed_ms
                        ),
                        Err(e) => eprintln!("codegraph: worktree index failed: {e}"),
                    }
                    std::sync::Arc::new(g)
                })
                .ok()
        })
        .await
        .map_err(|e| format!("worktree graph task failed: {e}"))?
    };
    spawn_agent_shared(
        factory,
        manager,
        agent_loops,
        name,
        initial_prompt,
        None,  // parentless — a main agent of its own
        None,  // no forced model — the default provider chain
        None,  // no role — an unrestricted main agent
        false, // own_plans_dir — the worktree binding drives the plans dir
        Some(WorktreeBinding {
            worktree,
            codegraph,
        }),
        // 3a.5: keep the factory build's default policy (pre-3a.5 caller)
        None,
        // 3a.6 (G5): the IPC path never resumes from a transcript
        None,
        // 3a.7 (G7): the IPC path sets no session limits
        None,
        // 3c (G2): the IPC path registers no host tools
        vec![],
    )
    .await
}



/// The concrete [`AgentSpawner`] wired into the factory, letting an agent
/// start background agents via its `spawn_agent` tool.
///
/// Holds the same manager + factory + per-agent loop map as the IPC commands,
/// so a tool-spawned agent is indistinguishable from a UI-spawned one (it
/// shows up in the sidebar, gets its own workflow, and its events fan in).
/// Also holds the Tauri `AppHandle` so it can emit a `PromptDispatched`
/// event for the spawned agent's task — making the task appear as the first
/// user message in the new agent's window, as if typed by the prompt entry.
pub struct IpcSpawner {
    factory: Arc<AgentLoopFactory>,
    manager: Arc<tokio::sync::Mutex<AgentManager>>,
    agent_loops: crate::ipc::state::AgentLoopMap,
    app: tauri::AppHandle,
}

impl IpcSpawner {
    /// Create a spawner over the shared IPC state pieces.
    pub fn new(
        factory: Arc<AgentLoopFactory>,
        manager: Arc<tokio::sync::Mutex<AgentManager>>,
        agent_loops: crate::ipc::state::AgentLoopMap,
        app: tauri::AppHandle,
    ) -> Self {
        Self {
            factory,
            manager,
            agent_loops,
            app,
        }
    }
}

#[async_trait::async_trait]
impl AgentSpawner for IpcSpawner {
    async fn spawn(
        &self,
        name: &str,
        task: &str,
        role: Option<String>,
        permission: mnemo::agent::policy::PermissionPolicy,
    ) -> Result<AgentId, String> {
        // The plain spawn path has no parent and no forced model — delegate to
        // spawn_with_parent with both as None.
        self.spawn_with_parent(name, task, None, None, role, permission)
            .await
    }

    fn parent_aware(&self) -> Option<&dyn ParentAwareSpawner> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl ParentAwareSpawner for IpcSpawner {
    /// Spawn a background agent on behalf of the given parent agent (the one
    /// whose `spawn_agent` tool call triggered this). The child's handle
    /// records `parent_id` so the event forwarder can notify the parent when
    /// the child finishes its task. `parent_id: None` means no notification.
    /// `model` forces the spawned agent onto a specific model for every turn
    /// (the `spawn_agent` tool's `model` parameter); `None` uses the default.
    /// `role` optionally constrains the spawned agent's tool surface (e.g.
    /// `Some("reviewer")` for a read-only reviewer).
    async fn spawn_with_parent(
        &self,
        name: &str,
        task: &str,
        parent_id: Option<AgentId>,
        model: Option<mnemo::config::ModelRef>,
        role: Option<String>,
        // §5.4 :377: the parent's policy. Accepted for the 3a Engine's
        // SpawnOptions (a free parent's OverridePermissions reaches the
        // child there); today's children are factory-built, which wires
        // ApproveViaApi.
        _permission: mnemo::agent::policy::PermissionPolicy,
    ) -> Result<AgentId, String> {
        let (id, _) = spawn_agent_shared(
            self.factory.clone(),
            self.manager.clone(),
            self.agent_loops.clone(),
            name.to_string(),
            Some(task.to_string()),
            parent_id,
            model,
            role,
            false, // own_plans_dir — subagents share the main dir (can't mutate)
            None, // no worktree binding — subagents inherit the parent's root
        // 3a.5: keep the factory build's default policy (pre-3a.5 caller)
        None,
        // 3a.6 (G5): subagents never resume from a transcript
        None,
        // 3a.7 (G7): subagents set no session limits
        None,
        // 3c (G2): subagents register no host tools
        vec![],
    )
    .await?;
        // Emit a PromptDispatched event for the spawned agent so its task
        // shows as the first user message in its window — as if typed by the
        // prompt entry — before the agent's own streaming text arrives. This
        // mirrors how backlog-dispatched prompts are surfaced. Best-effort:
        // a failed emit is logged inside emit_prompt_dispatched, not fatal.
        emit_prompt_dispatched(&self.app, id, task, &[]);
        Ok(id)
    }
}

#[async_trait::async_trait]
impl mnemo::runtime::DescendantTracker for IpcSpawner {
    /// Whether the agent with `agent_id` has any running spawned descendants.
    ///
    /// Delegates to [`AgentManager::has_running_descendants`] (which walks the
    /// `parent_id` chain so a grandchild counts too). The manager lock is held
    /// only for the duration of the (synchronous) walk, then released — no
    /// await is performed while holding it.
    async fn has_running_descendants(&self, agent_id: mnemo::runtime::AgentId) -> bool {
        let mgr = self.manager.lock().await;
        mgr.has_running_descendants(agent_id)
    }
}

