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
use mnemo::runtime::agent::AgentTask;
use mnemo::runtime::channels::{AgentCommand, AgentHandle, AgentId};
use mnemo::runtime::{AgentManager, AgentSpawner, ParentAwareSpawner};
use mnemo::tool::{SafetyLevel, ToolCategory, ToolFilter};

use crate::ipc::agent::AgentInfo;
use crate::ipc::error::IpcError;
use crate::ipc::events::emit_prompt_dispatched;
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

/// Spawn a background agent: build its loop via the factory, spawn its task,
/// register it with the manager, and track its loop for `get_workflow_state`.
///
/// Shared by the `spawn_agent` Tauri command (the UI button), the
/// [`IpcSpawner`] (the agent's `spawn_agent` tool), and main-agent startup
/// (main.rs), so all three paths behave identically. When `initial_prompt` is
/// `Some`, it's sent as the agent's first turn right after registration.
/// `parent_id` is the agent that spawned this one via the tool (`None` for
/// UI-button spawns and the main agent) — used to notify the parent when this
/// agent finishes. Returns `(agent_id, name)`.
/// The parent's per-agent root binding for subagent inheritance (parallel
/// run-all worktree agents, plan ffd7a86f).
///
/// `Some((spec, plans_dir))` when the parent loop was built with a root
/// spec: a worktree agent's subagents — notably its closing-sequence
/// reviewer — must see the worktree's tree and diff, not the main tree's,
/// and their read-only plan mirror must show the worktree agent's plan
/// (the parent's plans dir), not the main agent's. `None` for every
/// factory-default parent — today's behavior.
async fn parent_root_binding(
    agent_loops: &crate::ipc::state::AgentLoopMap,
    parent_id: AgentId,
) -> Option<(mnemo::agent::factory::AgentRootSpec, std::path::PathBuf)> {
    agent_loops.lock().await.get(&parent_id).and_then(|l| {
        l.root_spec()
            .cloned()
            .map(|spec| (spec, l.plans_dir().to_path_buf()))
    })
}

/// A parallel run-all worktree agent's build binding (plan ffd7a86f):
/// the item's git worktree root + the code graph over it. The spawned
/// agent is PARENTLESS (a main agent of its own — the run's per-agent
/// resolution keys on it), bound to the worktree via
/// [`AgentLoopFactory::build_with_root_spec`].
pub(crate) struct WorktreeBinding {
    /// The item's worktree root (a fresh checkout of `main` on the item's
    /// own `wt/runall-*` branch).
    pub worktree: std::path::PathBuf,
    /// The code graph over the worktree tree (a fresh graph over the
    /// worktree's `.coding/codegraph.db`, one index pass at spawn — the DB
    /// seeded from the main tree's when available, see
    /// [`spawn_run_all_agent`]). `None` on a graph-open failure — the
    /// `graph_*` tools are omitted for that agent, same as a main-tree
    /// open failure.
    pub codegraph: Option<std::sync::Arc<mnemo::codegraph::CodeGraph>>,
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
            mnemo::project::Project::from_root(g.root().to_path_buf()).codegraph_db
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
    )
    .await
}

pub(crate) async fn spawn_agent_shared(
    factory: Arc<AgentLoopFactory>,
    manager: Arc<tokio::sync::Mutex<AgentManager>>,
    agent_loops: crate::ipc::state::AgentLoopMap,
    name: String,
    initial_prompt: Option<String>,
    parent_id: Option<AgentId>,
    model: Option<mnemo::config::ModelRef>,
    role: Option<String>,
    own_plans_dir: bool,
    worktree: Option<WorktreeBinding>,
) -> Result<(AgentId, String), String> {
    // Allocate the agent id from the manager FIRST, then build the loop with
    // that id — so the agent's own `spawn_agent` tool records it as the parent
    // of any background agents it spawns (the completion-notification loop).
    let agent_id = {
        let mgr = manager.lock().await;
        mgr.next_id()
    };

    // Per the user's rule: completed (inactive) subagents are cleaned up only
    // when a NEW agent is spawned (or the user closes a tab) — never
    // proactively. Cancel every inactive subagent now, before registering the
    // new one, so their tabs disappear as the new agent appears. RUNNING
    // subagents are left alone (they exit on their own). The new agent's id is
    // passed as the trigger so it is never cancelled (it isn't registered yet,
    // but this is defensive).
    crate::ipc::events::cleanup_inactive_subagents(&manager, agent_id).await;

    // Build the per-agent loop BEFORE doing channel/spawn work. The factory
    // gives this agent its own Workflow + ToolRegistry, stamped with its id.
    // A UI-spawned parentless agent gets its own plans dir
    // (.coding/plans/agents/<id>/) so its plans don't clobber the main
    // agent's; the main agent + subagents share the main dir.
    // A subagent inherits its parent's per-agent root override (parallel
    // run-all worktree agents, plan ffd7a86f) — see parent_root_binding.
    let parent_binding = match parent_id {
        Some(pid) => parent_root_binding(&agent_loops, pid).await,
        None => None,
    };
    let agent_loop = if let Some((spec, parent_plans_dir)) = parent_binding {
        factory.build_with_root_spec(agent_id, parent_plans_dir, spec)
    } else if let Some(binding) = worktree {
        // A parallel run-all worktree agent (plan ffd7a86f): PARENTLESS,
        // bound to its own git worktree — the spec's sandbox/project root
        // + graph all point at the worktree, and its plans dir lives
        // INSIDE the worktree so plan bookkeeping lands on the item's own
        // branch.
        let plans_dir = binding
            .worktree
            .join(".coding")
            .join("plans")
            .join("agents")
            .join(agent_id.to_string());
        let spec = mnemo::agent::factory::AgentRootSpec {
            sandbox: mnemo::tool::agent::sandbox::Sandbox::new(&binding.worktree)
                .map_err(|e| format!("worktree sandbox: {e}"))?,
            project_root: binding.worktree.clone(),
            codegraph: binding.codegraph,
        };
        factory.build_with_root_spec(agent_id, plans_dir, spec)
    } else if own_plans_dir {
        let plans_dir = factory.plans_dir().join(format!("agents/{agent_id}"));
        factory.build_with_id_and_plans_dir(agent_id, plans_dir)
    } else {
        factory.build_with_id(agent_id)
    };

    // Force the agent onto a specific model for every turn when one was
    // requested (the `spawn_agent` tool's `model` parameter). This overrides
    // the normal subagent/state/skill resolution chain. Done before the
    // is_subagent block so the override is in place regardless of parentage.
    if let Some(model_ref) = model {
        agent_loop.set_forced_model(model_ref);
    }

    if parent_id.is_some() {
        // Sub-agent under main-only plan policy: deny plan mutations.
        agent_loop.set_plan_mutations_allowed(false);
        let workflow_arc = agent_loop.workflow_handle();
        let mut wf = workflow_arc.lock().await;
        wf.set_plan_mutations_allowed(false);
        // Stamp the dedicated sub-agent role state: build_inner's load_latest
        // derived the MAIN plan's lifecycle state (Executing/Reviewing/…)
        // from the shared plans dir, but a sub-agent is a single-task worker,
        // not a plan-lifecycle owner. The plan stack stays loaded (the
        // read-only mirror that feeds `current_plan` for reviewers and the UI
        // staircase); only the state is replaced. The tool allow-list below
        // completes the Subagent invariants before registration — no turn
        // runs before then, so `allowed_tools` is never consulted in between.
        wf.enter_subagent_state();
        // Mark the loop as a subagent so the per-context model resolver
        // considers the `[models.subagent]` override (e.g. a fast coding model
        // for background agents, distinct from the main agent's model).
        agent_loop.set_is_subagent(true);
    }

    // Compute the subagent's tool allow-list as a SUBSET of the spawning
    // (parent) agent's current permissions — the refined subagent permission
    // model: a subagent can never have a tool the parent doesn't currently
    // have (e.g. a subagent spawned from Planning can't reach file_edit, since
    // Planning hides it). Roles restrict further (reviewer = read-only ∩
    // parent). EXCEPTION: read-only/AutoRun role tools (git_read,
    // write_review_report) are always kept even if the parent lacks them —
    // they're safe (sandboxed output / read-only diff). The intersection is a
    // snapshot of the parent's permissions at spawn time; it doesn't track
    // parent state changes mid-spawn.
    let allowlist = compute_subagent_allowlist(&agent_loops, parent_id, &role).await;
    if let Some(list) = allowlist {
        let workflow_arc = agent_loop.workflow_handle();
        let mut wf = workflow_arc.lock().await;
        if role.as_deref() == Some("reviewer") {
            // A reviewer allow-list is enforced STRICTLY (ToolFilter::Reviewer:
            // only the listed tools + current_plan are visible — no auto-granted
            // memory/ask/backlog tools), so a reviewer can query but never
            // mutate memory, the backlog, the plan, or the review.
            wf.set_reviewer_allowlist(Some(list));
        } else {
            wf.set_tool_allowlist(Some(list));
        }
    }

    // A clone of the manager's fan-in sender, used below (outside the lock) to
    // emit the registration ContextUsage. Declared here so it outlives the
    // manager-lock block.
    let fanin_tx;
    {
        let mut mgr = manager.lock().await;
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(64);
        fanin_tx = mgr.fanin_sender();
        let task = AgentTask::new(agent_id, name.clone(), agent_loop.clone());
        tauri::async_runtime::spawn(task.run(cmd_rx, fanin_tx.clone()));
        let mut handle = AgentHandle::new(agent_id, name.clone(), cmd_tx);
        if let Some(pid) = parent_id {
            handle = handle.with_parent(pid);
        }
        handle = handle.with_role(role.clone());
        mgr.register(handle);
    }

    // Seed the UI's ctx bar from the start: emit an initial ContextUsage
    // (used = 0, max = this agent's context window) at registration so the
    // InflightBar shows "0 / <max>" before the first turn — mirroring the
    // /new clear-context emission (runtime/agent.rs). Deliberately sent OUTSIDE
    // the manager-lock block above: the fan-in channel is bounded (cap 256) and
    // the send awaits capacity, while the event forwarder takes the manager
    // lock in several per-event arms — awaiting the send under the lock could
    // deadlock both when the channel is full. The send also buffers even when
    // the forwarder isn't running yet (the main agent spawns before it). The
    // failure is swallowed (`let _`) — a dropped event only means the ctx bar
    // fills on the first turn instead.
    let max = agent_loop.context_manager().max_tokens() as u32;
    let _ = fanin_tx
        .send((
            agent_id,
            mnemo::runtime::AgentEvent::ContextUsage {
                used: 0,
                max,
                breakdown: mnemo::runtime::ContextBreakdown::default(),
                // The grade helper is `pub(crate)` to mnemo, so this app-side
                // seed event (fired before the first turn) stays ungraded — the
                // first turn's own ContextUsage carries the report.
                quality: None,
            },
        ))
        .await;

    // Track the new agent's loop so get_workflow_state(agent_id) can read its
    // plan. The event forwarder removes this entry on `Exited`.
    agent_loops.lock().await.insert(agent_id, agent_loop);

    // Kick off the first turn (the agent's `spawn_agent` tool passes its task
    // here). Sent after registration so the manager can route it.
    if let Some(text) = initial_prompt {
        manager
            .lock()
            .await
            .send(
                agent_id,
                AgentCommand::Prompt {
                    text,
                    images: vec![],
                },
            )
            .map_err(|e| format!("failed to send initial prompt to new agent: {e:?}"))?;
    }

    Ok((agent_id, name))
}

/// The reviewer role's base tool list — read-only tools + the read-only role
/// tools (`git_read`, `write_review_report`) + read-only queries (git history,
/// URL fetch, code-graph, memory/backlog retrieval). Intersected with the
/// parent's current permissions at spawn time, but the read-only role tools
/// are always kept (they're `AutoRun` / sandboxed, so granting them can't
/// violate "subagent ⊆ parent").
///
/// Enforced as a STRICT allow-list via `ToolFilter::Reviewer` (set through
/// `Workflow::set_reviewer_allowlist`): nothing is auto-granted beyond this
/// list + `current_plan`, so a reviewer can NEVER reach mutations of any
/// kind — memory writes, backlog mutations, ask_user, plan tools, finish,
/// file writes, shell, git, spawn_agent, skill tools, browser control.
const REVIEWER_BASE_TOOLS: &[&str] = &[
    // File/read + search.
    "file_read",
    "read_files",
    "search",
    "search_read",
    // Code knowledge graph (read-only symbol queries).
    "graph_search",
    "graph_context",
    "graph_impact",
    "graph_path",
    // Vision (read-only image analysis).
    "image_analysis",
    "image_analyze_chart",
    "image_diagnose_error",
    "image_extract_text",
    "image_ui_diff",
    "image_ui_to_artifact",
    "image_understand_diagram",
    // Read-only discovery.
    "list_models",
    // One read-only view into git (op = diff | log | show | status) — the reviewer's
    // only way to see uncommitted changes, since it has no shell/git.
    "git_read",
    "web_fetch",
    "backlog_list",
    // Memory retrieval (QUERY ONLY — never memory_write/update/supersede/
    // delete/consolidate).
    // One read path: search or browse, any record type. Replaced
    // memory_recall / memory_list / plans_search / reviews_search /
    // past_fixes / context_pack, which differed only by parameter values.
    "memory_search",
    // The reviewer's single output channel.
    "write_review_report",
];

/// Read-only role tools that are always granted to a role even if the parent
/// lacks them — they're `AutoRun` (sandboxed output / read-only diff), so
/// granting them can never make the subagent more permissive than the parent.
const ALWAYS_SAFE_ROLE_TOOLS: &[&str] = &["git_read", "write_review_report"];

/// Compute the subagent's tool allow-list as a subset of the spawning parent's
/// current permissions.
///
/// Returns `Some(list)` when a restriction should be applied:
/// - **Reviewer role** — the reviewer base list, intersected with the parent's
///   current allowed tool set, but always keeping the read-only role tools
///   (`git_read`, `write_review_report`) even if the parent lacks them.
/// - **Unrestricted spawn with a parent** — exactly the parent's current
///   allowed tool set (so a subagent spawned from Planning can't reach
///   `file_edit`, since Planning hides it).
///
/// Returns `None` (no restriction) ONLY when there is no parent (a UI-button
/// spawn or the main agent — unrestricted). A KNOWN `parent_id` whose loop is
/// missing from the map returns `Some(vec![])` (deny all tools) — fail-closed,
/// so a missing parent loop can never silently escalate the subagent to the
/// full tool set.
///
/// The intersection is a snapshot of the parent's permissions at spawn time;
/// it does not track parent state changes mid-spawn.
///
/// **How the subset is computed (the security-critical property):** for each
/// tool the parent's registry registers, we read its *actual* `category()` +
/// `safety()` off the `&dyn Tool` and ask the parent's current `ToolFilter`
/// whether it admits that exact `(category, safety, name)`. This is the only
/// sound way to consult the filter — a name-only check would short-circuit on
/// the `ToolCategory::Memory` arm (which is unconditionally `true` for every
/// name) and grant the subagent tools the parent's state hides. Memory tools
/// MUST be listed for a reviewer: the child's `ToolFilter::Reviewer` arm
/// grants nothing implicitly, so a memory tool omitted from the allow-list is
/// genuinely unavailable (query-only retrieval is in `REVIEWER_BASE_TOOLS`;
/// memory writes are not).
async fn compute_subagent_allowlist(
    agent_loops: &crate::ipc::state::AgentLoopMap,
    parent_id: Option<AgentId>,
    role: &Option<String>,
) -> Option<Vec<String>> {
    let parent_id = parent_id?;

    // Snapshot the parent's current ToolFilter + the set of tools its registry
    // actually registers, each paired with its real category + safety (read off
    // the &dyn Tool so the filter is consulted with the exact tuple, not a
    // name-only guess). The filter + registry are read under their respective
    // locks, then released before we build the allow-list (no lock held across
    // the intersection).
    let (filter, registered): (ToolFilter, Vec<(String, ToolCategory, SafetyLevel)>) = {
        let loops = agent_loops.lock().await;
        // Fail CLOSED: a KNOWN parent_id whose loop is missing from the map
        // returns an empty allow-list (deny all tools) — NOT None (which would
        // mean "no restriction" and grant the subagent the full tool set). The
        // parent's loop should always be present for a tool-spawned subagent;
        // if it isn't, something is wrong, so deny everything rather than
        // silently escalating.
        let parent = match loops.get(&parent_id) {
            Some(p) => p.clone(),
            None => {
                eprintln!("spawn: parent {parent_id} loop missing — denying all tools to subagent");
                return Some(Vec::new());
            }
        };
        drop(loops);
        let wf = parent.workflow_handle();
        let wf = wf.lock().await;
        let filter = wf.allowed_tools();
        // Collect owned (name, category, safety) tuples so the borrow of
        // `parent` ends here (the Arc is dropped at the block close).
        let registered = parent
            .tools()
            .iter()
            .map(|t| (t.name().to_string(), t.category(), t.safety()))
            .collect();
        (filter, registered)
    };

    // The parent's effective allowed set: a registered tool is parent-allowed
    // iff the parent's current filter admits it for its ACTUAL category +
    // safety. This is the security-critical check — it respects the parent's
    // workflow state (e.g. Planning hides NeedsApproval Agent tools like
    // file_write, so they're excluded).
    let parent_allowed: Vec<String> = registered
        .iter()
        .filter(|(name, category, safety)| filter.allows(*category, *safety, name))
        .map(|(name, _, _)| name.clone())
        .collect();

    match role.as_deref() {
        Some("reviewer") => {
            // Intersect the reviewer base list with the parent's allowed set,
            // but always keep the read-only role tools (they're AutoRun / safe:
            // git_read is read-only, write_review_report writes only
            // under .coding/reviews/). Granting them can never make the
            // subagent more permissive than the parent.
            let out: Vec<String> = REVIEWER_BASE_TOOLS
                .iter()
                .filter_map(|name| {
                    if parent_allowed.iter().any(|n| n == *name)
                        || ALWAYS_SAFE_ROLE_TOOLS.contains(name)
                    {
                        Some((*name).to_string())
                    } else {
                        None
                    }
                })
                .collect();
            // REVIEWER_BASE_TOOLS is a fixed literal with no duplicates, and
            // each entry is visited once, so the result has no duplicates — no
            // dedup needed.
            Some(out)
        }
        Some(other) => {
            // Unknown role — refuse to spawn an unrestricted agent. The
            // caller (spawn_agent_shared) already validated the role before
            // reaching here, so this is defensive; return an empty list so
            // the subagent gets no tools rather than the parent's full set.
            eprintln!("spawn: unknown role '{other}' reached allowlist compute");
            Some(Vec::new())
        }
        None => {
            // Unrestricted spawn with a parent: the subagent gets exactly the
            // parent's current allowed set (a subset by construction).
            Some(parent_allowed)
        }
    }
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
    async fn spawn(&self, name: &str, task: &str, role: Option<String>) -> Result<AgentId, String> {
        // The plain spawn path has no parent and no forced model — delegate to
        // spawn_with_parent with both as None.
        self.spawn_with_parent(name, task, None, None, role).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::agent::AgentLoop;
    use mnemo::tool::ToolRegistry;
    use mnemo::workflow::Workflow;

    #[test]
    fn reviewer_base_tools_never_name_plan_mutations() {
        // Backlog 24e1c98e (2026-12-23): update_plan is now fully callable in
        // Reviewing for the main agent — the reviewer subagent must never see
        // it. Structural guarantee: the reviewer's base list never names any
        // plan-mutation tool (the strict ToolFilter::Reviewer allow-list
        // auto-grants nothing), and every subagent additionally runs with
        // plan_mutations_allowed(false). This pins the base-list half against
        // future edits.
        for tool in [
            "update_plan",
            "create_plan",
            "complete_step",
            "abandon_plan",
            "finish",
        ] {
            assert!(
                !REVIEWER_BASE_TOOLS.contains(&tool),
                "REVIEWER_BASE_TOOLS must never name {tool} — the reviewer is read-only"
            );
        }
    }

    // NOTE: there is deliberately no unit test for the registration-time
    // ContextUsage emission in spawn_agent_shared (used=0, max=window). The
    // harness here builds AgentLoop directly (see parent_in_state below) and
    // has no AgentLoopFactory, which spawn_agent_shared requires — and the
    // send depends on `tauri::async_runtime`, which isn't available in these
    // lib-only tests. The emission mirrors the /new clear-context emission
    // (src/runtime/agent.rs:561-573), the frontend seed path has its own
    // store test (useAgentStore.test.ts seedContextCaps), and the payload
    // shape is verified by the event fixture contract test
    // (contract_fixtures.rs).

    /// Build an `AgentLoop` in a given workflow state, register a realistic
    /// tool set (file_read + file_write + git + git_read + write_review_report
    /// + create_plan + complete_step + web_fetch +
    /// memory_recall), and insert it into an `agent_loops` map under
    /// `parent_id`. Returns the map + the loop's workflow handle so the test
    /// can drive state transitions.
    async fn parent_in_state(
        parent_id: AgentId,
        state: mnemo::workflow::WorkflowState,
    ) -> (
        Arc<tokio::sync::Mutex<std::collections::HashMap<AgentId, Arc<AgentLoop>>>>,
        Arc<tokio::sync::Mutex<Workflow>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(mnemo::tool::agent::sandbox::Sandbox::new(dir.path()).unwrap());
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        // Drive the workflow into the requested state.
        {
            let mut wf = workflow.lock().await;
            match state {
                mnemo::workflow::WorkflowState::Planning => {} // default
                mnemo::workflow::WorkflowState::Executing => {
                    wf.create_plan("p", "g", "c", vec!["s".into()]).unwrap();
                }
                mnemo::workflow::WorkflowState::Complete => {
                    wf.create_plan("p", "g", "c", vec!["s".into()]).unwrap();
                    wf.complete_step(0).unwrap();
                    wf.finish().unwrap();
                }
                _ => {} // Reviewing/Skill not needed for these tests
            }
        }
        // Register a realistic tool set so the parent's registry has names to
        // intersect against.
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(mnemo::tool::agent::file_read::FileReadTool::new(
            (*sandbox).clone(),
        )));
        registry.register(Box::new(
            mnemo::tool::agent::file_write::FileWriteTool::new((*sandbox).clone()),
        ));
        registry.register(Box::new(mnemo::tool::agent::git::GitTool::new(dir.path())));
        registry.register(Box::new(
            mnemo::tool::agent::git_read_tool::GitReadTool::new(dir.path()),
        ));
        registry.register(Box::new(
            mnemo::tool::agent::write_review_report::WriteReviewReportTool::new(
                dir.path().join("reviews"),
            ),
        ));
        registry.register(Box::new(mnemo::tool::workflow::plan::CreatePlanTool::new(
            workflow.clone(),
        )));
        registry.register(Box::new(
            mnemo::tool::workflow::plan::CompleteStepTool::new(workflow.clone()),
        ));
        // Read-only queries the reviewer's base list names — registered so the
        // intersection keeps them (the production main agent registers these).
        registry.register(Box::new(mnemo::tool::agent::web_fetch::WebFetchTool::new()));
        let store: Arc<dyn mnemo::memory::MemoryStoreTrait> = Arc::new(
            mnemo::memory::MemoryStore::open_in_memory(Arc::new(
                mnemo::memory::embedder::HashEmbedder::new(),
            ))
            .unwrap(),
        );
        registry.register(Box::new(
            mnemo::tool::memory::retrieval::MemorySearchTool::new(store),
        ));
        // Code-graph read tools (the reviewer's base list names all four).
        let graph = Arc::new(
            mnemo::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap(),
        );
        registry.register(Box::new(
            mnemo::tool::agent::codegraph::GraphSearchTool::new(graph.clone()),
        ));
        registry.register(Box::new(
            mnemo::tool::agent::codegraph::GraphContextTool::new(graph.clone()),
        ));
        registry.register(Box::new(
            mnemo::tool::agent::codegraph::GraphImpactTool::new(graph.clone()),
        ));
        registry.register(Box::new(mnemo::tool::agent::codegraph::GraphPathTool::new(
            graph,
        )));
        // The read-only backlog query (the reviewer's base list names it).
        registry.register(Box::new(
            mnemo::tool::workflow::backlog::BacklogListTool::new(Arc::new(
                tokio::sync::Mutex::new(mnemo::backlog::BacklogStore::open(
                    dir.path().join("backlog.jsonl"),
                )),
            )),
        ));
        let provider: Arc<dyn mnemo::provider::LlmClient> = Arc::new(NoopProvider);
        let agent = AgentLoop::new(
            mnemo::agent::AgentLoopConfig {
                provider,
                tools: Arc::new(registry),
                workflow: workflow.clone(),
                sandbox,
                safety_mode: mnemo::config::SafetyMode::Autonomous,
                context_manager: mnemo::agent::context::ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            mnemo::project::Constitution::default(),
        )
        .with_agent_id(parent_id);

        let map = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        map.lock().await.insert(parent_id, Arc::new(agent));
        (map, workflow)
    }

    #[tokio::test]
    async fn parent_root_binding_returns_spec_and_plans_dir() {
        // Parallel run-all (plan ffd7a86f): a subagent of a root-spec
        // (worktree) agent inherits the parent's root spec AND plans dir;
        // a factory-default parent yields None (today's behavior).
        let dir = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        // Canonicalize so the sandbox's canonicalized root compares equal
        // (Windows `\\?\` prefixes).
        let worktree_root = worktree.path().canonicalize().unwrap();
        let plans_dir = worktree_root.join("plans/agents/7");
        let spec = mnemo::agent::factory::AgentRootSpec {
            sandbox: mnemo::tool::agent::sandbox::Sandbox::new(&worktree_root).unwrap(),
            project_root: worktree_root.clone(),
            codegraph: None,
        };
        // Build a parent loop the way build_with_root_spec does: the spec
        // stamped on the loop + the worktree plans dir.
        let parent = AgentLoop::new(
            mnemo::agent::AgentLoopConfig {
                provider: Arc::new(NoopProvider),
                tools: Arc::new(ToolRegistry::new()),
                workflow: Arc::new(tokio::sync::Mutex::new(Workflow::new(
                    plans_dir.clone(),
                ))),
                sandbox: Arc::new(
                    mnemo::tool::agent::sandbox::Sandbox::new(dir.path()).unwrap(),
                ),
                safety_mode: mnemo::config::SafetyMode::Autonomous,
                context_manager: mnemo::agent::context::ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            mnemo::project::Constitution::default(),
        )
        .with_agent_id(7)
        .with_plans_dir(plans_dir.clone())
        .with_root_spec(Some(spec.clone()));

        let map = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        map.lock().await.insert(7, Arc::new(parent));

        let (got_spec, got_plans) =
            parent_root_binding(&map, 7).await.expect("the parent's root binding");
        assert_eq!(got_spec.sandbox.root(), worktree_root);
        assert_eq!(got_spec.project_root, worktree_root);
        assert!(got_spec.codegraph.is_none());
        assert_eq!(got_plans, plans_dir);

        // A factory-default parent (no root spec) yields None.
        let plain = AgentLoop::new(
            mnemo::agent::AgentLoopConfig {
                provider: Arc::new(NoopProvider),
                tools: Arc::new(ToolRegistry::new()),
                workflow: Arc::new(tokio::sync::Mutex::new(Workflow::new(
                    dir.path().join("plans"),
                ))),
                sandbox: Arc::new(
                    mnemo::tool::agent::sandbox::Sandbox::new(dir.path()).unwrap(),
                ),
                safety_mode: mnemo::config::SafetyMode::Autonomous,
                context_manager: mnemo::agent::context::ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            mnemo::project::Constitution::default(),
        )
        .with_agent_id(8);
        let map2 = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        map2.lock().await.insert(8, Arc::new(plain));
        assert!(
            parent_root_binding(&map2, 8).await.is_none(),
            "a factory-default parent has no root binding"
        );
    }

    /// A provider that never completes — these tests never run a turn, so it's
    /// never actually called. Returns an error if ever invoked (rather than
    /// `unimplemented!`, to avoid needing the `futures` crate as a direct dep
    /// just for the `BoxStream` type).
    struct NoopProvider;
    #[async_trait::async_trait]
    impl mnemo::provider::LlmClient for NoopProvider {
        fn capabilities(&self) -> &mnemo::provider::Capabilities {
            // A static openai caps — never read in these tests.
            use std::sync::OnceLock;
            static CAPS: OnceLock<mnemo::provider::Capabilities> = OnceLock::new();
            CAPS.get_or_init(mnemo::provider::Capabilities::openai)
        }
        fn kind(&self) -> mnemo::provider::ProviderKind {
            mnemo::provider::ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "noop"
        }
        async fn complete(
            &self,
            _: &[mnemo::provider::Message],
            _: &[mnemo::provider::ToolSchema],
            _: Option<mnemo::provider::ToolChoice>,
        ) -> mnemo::error::Result<futures::stream::BoxStream<'_, mnemo::provider::LlmEvent>>
        {
            Err(mnemo::error::Error::Provider(
                "noop provider never completes".into(),
            ))
        }
    }

    #[tokio::test]
    async fn reviewer_allowlist_intersected_with_parent_planning() {
        // A reviewer spawned from a parent in Planning: the parent's filter
        // hides file_write/git (mutations), so the reviewer's allow-list must
        // NOT contain them — even though they're in the reviewer's base list
        // (git isn't, but file_write-adjacent tools are). Crucially the
        // read-only role tools (git_read, write_review_report) ARE kept even
        // though the parent lacks them (they're AutoRun / safe).
        let (agent_loops, _wf) = parent_in_state(1, mnemo::workflow::WorkflowState::Planning).await;
        let list = compute_subagent_allowlist(&agent_loops, Some(1), &Some("reviewer".into()))
            .await
            .expect("reviewer spawn with a parent returns a list");

        // Read tools the parent allows (Planning allows AutoRun Agent tools).
        assert!(list.contains(&"file_read".to_string()), "file_read allowed");
        // The read-only role tools are always kept (AutoRun / safe).
        assert!(
            list.contains(&"git_read".to_string()),
            "git_read always kept for a reviewer (read-only role tool)"
        );
        assert!(
            list.contains(&"write_review_report".to_string()),
            "write_review_report always kept for a reviewer (read-only role tool)"
        );
        // Read-only queries the reviewer legitimately needs are in the base
        // list (intersected with what the parent allows in Planning).
        assert!(list.contains(&"git_read".to_string()), "git_read allowed");
        assert!(list.contains(&"web_fetch".to_string()), "web_fetch allowed");
        assert!(
            list.contains(&"memory_search".to_string()),
            "memory_search allowed (read-only retrieval)"
        );
        assert!(
            list.contains(&"backlog_list".to_string()),
            "backlog_list allowed (read-only backlog query)"
        );
        // Mutations the parent hides in Planning must NOT leak to the child.
        assert!(
            !list.contains(&"file_write".to_string()),
            "file_write must not leak to a reviewer spawned from Planning"
        );
        assert!(
            !list.contains(&"git".to_string()),
            "git must not leak to a reviewer spawned from Planning"
        );
    }

    #[tokio::test]
    async fn reviewer_allowlist_includes_mutations_parent_has_in_executing() {
        // A reviewer spawned from a parent in Executing: the parent's filter
        // allows file_write/git, but the reviewer role RESTRICTS to read-only,
        // so they still must NOT appear. This proves the role restricts
        // further than the parent (reviewer = read-only ∩ parent).
        let (agent_loops, _wf) =
            parent_in_state(2, mnemo::workflow::WorkflowState::Executing).await;
        let list = compute_subagent_allowlist(&agent_loops, Some(2), &Some("reviewer".into()))
            .await
            .expect("reviewer spawn with a parent returns a list");

        assert!(list.contains(&"file_read".to_string()));
        assert!(list.contains(&"git_read".to_string()));
        assert!(list.contains(&"write_review_report".to_string()));
        // Even though the parent (Executing) allows file_write + git, the
        // reviewer role excludes them — they're not in the reviewer base list.
        assert!(
            !list.contains(&"file_write".to_string()),
            "reviewer role must not include file_write even if parent has it"
        );
        assert!(
            !list.contains(&"git".to_string()),
            "reviewer role must not include git even if parent has it"
        );
        // The reviewer contract: query, never mutate. Every mutation channel
        // is excluded even when the parent (Executing) would allow it.
        for mutation in [
            "memory_write",
            "memory_update",
            "memory_supersede",
            "memory_delete",
            "memory_consolidate",
            "backlog_add",
            "backlog_status",
            "ask_user",
            "create_plan",
            "update_plan",
            "complete_step",
            "abandon_plan",
            "finish",
            "skill_start",
            "skill_end",
            "abandon_skill",
            "skill_create",
            "skill_reload",
            "spawn_agent",
            "file_edit",
            "shell",
            "browser_eval",
        ] {
            assert!(
                !list.contains(&mutation.to_string()),
                "reviewer role must not include {mutation}"
            );
        }
        // The read-only queries are present (intersected with Executing).
        assert!(list.contains(&"git_read".to_string()));
        assert!(list.contains(&"web_fetch".to_string()));
        assert!(list.contains(&"graph_search".to_string()));
        assert!(list.contains(&"memory_search".to_string()));
    }

    #[tokio::test]
    async fn unrestricted_spawn_gets_parent_subset() {
        // An unrestricted spawn (no role) from a parent in Planning gets
        // EXACTLY the parent's current allowed set — so file_write/git are
        // absent (Planning hides them) but file_read + create_plan are
        // present. This is the core "subagent ⊆ parent" property.
        let (agent_loops, _wf) = parent_in_state(3, mnemo::workflow::WorkflowState::Planning).await;
        let list = compute_subagent_allowlist(&agent_loops, Some(3), &None)
            .await
            .expect("unrestricted spawn with a parent returns a list");

        assert!(
            list.contains(&"file_read".to_string()),
            "parent allows file_read"
        );
        assert!(
            list.contains(&"create_plan".to_string()),
            "parent allows create_plan in Planning"
        );
        assert!(
            !list.contains(&"file_write".to_string()),
            "subagent must not get file_write (parent Planning hides it)"
        );
        assert!(
            !list.contains(&"git".to_string()),
            "subagent must not get git (parent Planning hides it)"
        );
        assert!(
            !list.contains(&"write_review_report".to_string()),
            "an unrestricted subagent must never get write_review_report — it is visible under \
             NO base-state filter (reviewer-only authorship; even a subagent of the main agent \
             can never author a review)"
        );
    }

    #[tokio::test]
    async fn no_parent_returns_none() {
        // A UI-button spawn (no parent) is unrestricted — None means "apply
        // no allow-list override, use the state-derived filter".
        let map = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let list = compute_subagent_allowlist(&map, None, &Some("reviewer".into())).await;
        assert!(
            list.is_none(),
            "no parent → no restriction (None), got: {:?}",
            list
        );
    }

    #[tokio::test]
    async fn missing_parent_loop_denies_all() {
        // C1 fail-closed: a KNOWN parent id whose loop is missing from the map
        // returns an empty allow-list (deny all tools) — NOT None (which would
        // mean "no restriction" and grant the subagent the full tool set). The
        // parent's loop should always be present for a tool-spawned subagent;
        // if it isn't, deny everything rather than silently escalating.
        let map = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let list = compute_subagent_allowlist(&map, Some(999), &Some("reviewer".into())).await;
        assert!(
            list.as_ref().is_some_and(|v| v.is_empty()),
            "missing parent loop → deny all (Some(vec![])), got: {:?}",
            list
        );
    }
}
