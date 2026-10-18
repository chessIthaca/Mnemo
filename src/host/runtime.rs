// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The shared agent spawn path and lifecycle bookkeeping used by every
//! frontend: id allocation, loop construction (main tree, per-agent plans dir,
//! worktree root), subagent allow-lists, task spawn, registration, and the
//! parent-notification bookkeeping. Moved from the app crate.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::agent::factory::AgentLoopFactory;
use crate::agent::AgentLoop;
use crate::runtime::agent::AgentTask;
use crate::runtime::channels::{AgentCommand, AgentEvent, AgentHandle, AgentId};
use crate::runtime::{AgentManager, AgentSpawner, DescendantTracker, ParentAwareSpawner};
use crate::tool::{SafetyLevel, ToolCategory, ToolFilter};

/// The per-agent loop map: `Arc<tokio::Mutex<HashMap<AgentId, Arc<AgentLoop>>>>`.
/// Shared by the spawn path, the event forwarders and the IPC state.
pub type AgentLoopMap =
    Arc<tokio::sync::Mutex<std::collections::HashMap<AgentId, Arc<AgentLoop>>>>;
/// A parallel run-all worktree agent's build binding (plan ffd7a86f):
/// the item's git worktree root + the code graph over it. The spawned
/// agent is PARENTLESS (a main agent of its own — the run's per-agent
/// resolution keys on it), bound to the worktree via
/// [`AgentLoopFactory::build_with_root_spec`].
pub struct WorktreeBinding {
    /// The item's worktree root (a fresh checkout of `main` on the item's
    /// own `wt/runall-*` branch).
    pub worktree: std::path::PathBuf,
    /// The code graph over the worktree tree (a fresh graph over the
    /// worktree's `.coding/codegraph.db`, one index pass at spawn — the DB
    /// seeded from the main tree's when available, see
    /// [`spawn_run_all_agent`]). `None` on a graph-open failure — the
    /// `graph_*` tools are omitted for that agent, same as a main-tree
    /// open failure.
    pub codegraph: Option<std::sync::Arc<crate::codegraph::CodeGraph>>,
}

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
    agent_loops: &AgentLoopMap,
    parent_id: AgentId,
) -> Option<(crate::agent::factory::AgentRootSpec, std::path::PathBuf)> {
    agent_loops.lock().await.get(&parent_id).and_then(|l| {
        l.root_spec()
            .cloned()
            .map(|spec| (spec, l.plans_dir().to_path_buf()))
    })
}

/// The host dependencies a spawn needs: the loop factory (builds the
/// agent's loop), the manager (registers it), and the per-agent loop map
/// (tracks it for `get_workflow_state`). The three always travel
/// together from every spawn path — grouped so [`spawn_agent_shared`]
/// stays under the argument budget.
pub struct SpawnHost {
    /// Builds the agent's loop (tools, provider routing, brain wiring).
    pub factory: Arc<AgentLoopFactory>,
    /// Registers the agent + owns the task bookkeeping.
    pub manager: Arc<tokio::sync::Mutex<AgentManager>>,
    /// The live per-agent loop map.
    pub agent_loops: AgentLoopMap,
}

/// The per-session spawn options — everything except the spawn's identity
/// (name, initial prompt, parent). `None`/`false`/`vec![]` defaults keep
/// the pre-3a.5 behavior; each field is applied post-build exactly as the
/// corresponding positional argument was.
pub struct SessionOptions {
    /// A forced model — `None` = the default routing.
    pub model: Option<crate::config::ModelRef>,
    /// The workflow role (restricts the tool surface) — `None` = unrestricted.
    pub role: Option<String>,
    /// Whether the agent owns its `.coding/plans/agents/<id>/` dir.
    pub own_plans_dir: bool,
    /// The worktree binding (root override) — `None` = the main tree.
    pub worktree: Option<WorktreeBinding>,
    /// 3a.5 (§5.4 :303-314): the per-session permission policy — `None`
    /// keeps the factory build's default (every pre-3a.5 caller); `Some`
    /// is applied post-build via [`AgentLoop::set_permission`] (the
    /// Engine resolves the engine default vs. the SpawnOptions override).
    pub permission: Option<crate::agent::policy::PermissionPolicy>,
    /// 3a.6 (G5, §5.9.5): the resume seed — the transcript's messages
    /// become the initial conversation (`None` = a fresh session).
    pub initial_messages: Option<Vec<crate::provider::Message>>,
    /// 3a.7 (G7, §5.9.7): the session limits — `None` keeps the loop's
    /// default (unlimited); `Some` is applied post-build via
    /// [`AgentLoop::set_limits`].
    pub limits: Option<crate::runtime::channels::SessionLimits>,
    /// 3c/G2 (§5.9.2): the session's host tools — spec shims plus any
    /// engine-registered custom tools. Registered post-build via the
    /// registry's shared dynamic slot (interior-mutable — the registry
    /// sits behind an Arc, so there is no `&mut` to be had); each is
    /// advertised from the first request and dispatch intercepts its
    /// calls via the `host_tool_timeout_ms` marker. `vec![]` for every
    /// pre-3c caller.
    pub host_tools: Vec<std::sync::Arc<dyn crate::tool::Tool>>,
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
pub async fn spawn_agent_shared(
    host: SpawnHost,
    name: String,
    initial_prompt: Option<String>,
    parent_id: Option<AgentId>,
    session: SessionOptions,
) -> Result<(AgentId, String), String> {
    let SpawnHost {
        factory,
        manager,
        agent_loops,
    } = host;
    let SessionOptions {
        model,
        role,
        own_plans_dir,
        worktree,
        permission,
        initial_messages,
        limits,
        host_tools,
    } = session;
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
    cleanup_inactive_subagents(&manager, agent_id).await;

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
        let spec = crate::agent::factory::AgentRootSpec {
            sandbox: crate::tool::agent::sandbox::Sandbox::new(&binding.worktree)
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

    // 3c/G2 (§5.9.2): register the session's host tools into the registry
    // via the shared dynamic slot — the late-registration path the MCP
    // reveal machinery uses. A shim carries no deferred group, so it is
    // advertised from the very first request, and `get` finds it for the
    // dispatch-time round-trip. Duplicate names across the spec shims +
    // engine-registered tools are the caller's problem (the engine checks
    // against the registry's built-ins at spawn).
    if !host_tools.is_empty() {
        let slot = agent_loop.tools.dynamic_slot();
        for tool in host_tools {
            if !slot.contains_name(tool.name()) {
                slot.push(tool);
            }
        }
    }

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
        // 2b.3: the two workflow writes ride the neutral handle's compound
        // stamp (the plan-mutation deny + the dedicated sub-agent role state
        // — the plan stack stays loaded as the read-only mirror that feeds
        // `current_plan` for reviewers and the UI staircase; only the state
        // is replaced). The tool allow-list below completes the Subagent
        // invariants before registration — no turn runs before then, so
        // `allowed_tools` is never consulted in between.
        agent_loop
            .workflow_handle()
            .stamp_subagent_invariants()
            .await;
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
        // 2b.3: the role allow-list write rides the neutral handle — the
        // reviewer arm enforces it STRICTLY (ToolFilter::Reviewer: only the
        // listed tools + current_plan are visible — no auto-granted memory/
        // ask/backlog tools), so a reviewer can query but never mutate
        // memory, the backlog, the plan, or the review.
        agent_loop
            .workflow_handle()
            .set_role_allowlist(role.as_deref() == Some("reviewer"), list)
            .await;
    }

    // 3a.5 (§5.4): apply the per-session permission override (the engine
    // resolves engine default vs. SpawnOptions) BEFORE the task starts —
    // the loop is built but not yet running, so the swap is race-free.
    if let Some(permission) = permission {
        agent_loop.set_permission(permission);
    }

    // 3a.7 (G7, §5.9.7): apply the per-session limits the same way —
    // post-build, pre-task, race-free. `set_limits` resets the counters
    // + starts the wall-clock epoch (the session's limits epoch).
    if let Some(limits) = limits {
        agent_loop.set_limits(limits);
    }

    // A clone of the manager's fan-in sender, used below (outside the lock) to
    // emit the registration ContextUsage. Declared here so it outlives the
    // manager-lock block.
    let fanin_tx;
    {
        let mut mgr = manager.lock().await;
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(64);
        fanin_tx = mgr.fanin_sender();
        let mut task = AgentTask::new(agent_id, name.clone(), agent_loop.clone());
        // 3a.6 (G5): the resume seed — applied before the task runs, so
        // the seeded history is the first request's byte prefix.
        if let Some(messages) = initial_messages {
            task.seed_messages(messages);
        }
        tokio::spawn(task.run(cmd_rx, fanin_tx.clone()));
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
            crate::runtime::AgentEvent::ContextUsage {
                used: 0,
                max,
                breakdown: crate::runtime::ContextBreakdown::default(),
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

/// Read-only role tools that are always granted to a role even if the parent lacks them — they're `AutoRun` (sandboxed output / read-only diff), so
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
    agent_loops: &AgentLoopMap,
    parent_id: Option<AgentId>,
    role: &Option<String>,
) -> Option<Vec<String>> {
    let parent_id = parent_id?;

    // Snapshot the parent's current ToolFilter + the set of tools its registry
    // actually registers, each paired with its real category + safety (read off
    // the &dyn Tool so the filter is consulted with the exact tuple, not a
    // name-only guess). Since 2a.7 the parent's allowed set comes from the
    // agent's policy (tool_surface().gate — WorkflowPolicy's delegate is the
    // same allowed_tools() read since 2a.3); None (a NoPolicy parent loop,
    // e.g. the test harness) keeps the raw workflow read. tool_surface()
    // takes and drops the parent's workflow lock inside the policy, so the
    // one-lock-at-a-time discipline is preserved and the registry snapshot
    // below needs no lock.
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
        // 2a.7: the parent's allowed set consults the agent's policy —
        // WorkflowPolicy::tool_surface's gate IS allowed_tools() (the 2a.3
        // delegate), so a policy-carrying parent filters identically; a
        // None gate (NoPolicy parent loops, the test harness) keeps the
        // raw workflow read, scoped inside the arm so the lock is never
        // held across the registry snapshot or the intersection.
        let filter = match parent.policy().tool_surface().await.gate {
            Some(filter) => filter,
            None => parent.workflow_handle().allowed_tools().await,
        };
        // Collect owned (name, category, safety) tuples so the borrow of
        // `parent` ends here (the Arc is dropped at the block close). No
        // lock is held across this snapshot.
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


/// Cancel every INACTIVE subagent registered with the manager, leaving the
/// given `trigger_agent` and any RUNNING subagent untouched.
///
/// Called when an agent starts a fresh plan from Complete (Complete →
/// Executing): stale background agents from the previous plan should be
/// cleaned up so their tabs disappear. A subagent is "inactive" when it has
/// started at least one turn and is not currently running — cancelling a
/// running subagent would kill a task still in flight, so those are left alone
/// (they exit on their own and are removed by the existing `Exited` forwarder
/// path), and a subagent that has never started is *pending*, not completed,
/// so it is left alone too.
///
/// Also called from `spawn_agent_shared` right before a new agent is spawned:
/// per the user's rule, completed (inactive) subagents are cleaned up only when
/// a new agent is spawned (or the user closes a tab) — never proactively. This
/// keeps finished subagent tabs around for review until the user moves on.
///
/// Each cancelled subagent emits `Finished` then `Exited`; the `Exited`
/// handler in the forwarder removes it from the manager + the per-agent loop
/// map and emits the event so the frontend drops its tab.
///
/// The snapshot + all the `Cancel` sends happen under a **single** manager
/// lock acquisition. `mgr.send` uses `try_send` (non-blocking), so this never
/// awaits while holding the lock. Holding the lock across the whole loop
/// prevents a concurrent `send_prompt` IPC command from enqueuing a `Prompt`
/// *between* the snapshot and the `Cancel` — the `Prompt` send needs the same
/// lock, so it blocks until every `Cancel` is already in the channel, landing
/// the `Cancel` ahead of any later `Prompt` in the FIFO. The agent task then
/// reads the `Cancel` first and exits without running an extra turn.
pub async fn cleanup_inactive_subagents(
    manager_arc: &Arc<Mutex<AgentManager>>,
    trigger_agent: AgentId,
) {
    let mgr = manager_arc.lock().await;
    let to_cancel: Vec<AgentId> = mgr
        .list()
        .iter()
        .filter(|h| {
            h.id != trigger_agent
                && h.parent_id.is_some()
                // "Inactive" means ran-and-went-idle, NOT never-started. A
                // subagent registered by an earlier `spawn_agent` in the SAME
                // assistant message has `running == false` until its first
                // `Started` event reaches the forwarder — without the
                // `has_ever_started` half of this test, each spawn's cleanup
                // cancelled its not-yet-started siblings, so only the
                // last-spawned agent of a batch survived (live-observed
                // 2026-09-08, plan ccff0138: four reviewers spawned, one ran).
                // Latch first (Acquire load): observing `has_ever_started`
                // guarantees the `running = true` store that preceded the
                // latch store in `set_running` is visible to the
                // `is_running()` load below — on the first latch transition
                // (a freshly spawned agent) the two can never present
                // "started + idle" mid-`set_running(true)` (quality review
                // Q1, 2026-09-08). On re-starts (turn 2+) a
                // nanosecond-scale residual remains, equivalent to the
                // cleanup winning the race during the agent's designed
                // idle gap between turns.
                && h.has_ever_started()
                && !h.is_running()
        })
        .map(|h| h.id)
        .collect();
    // Send each Cancel while still holding the lock (try_send is non-blocking,
    // so this is brief and never awaits). Keeping the lock until all Cancels
    // are enqueued guarantees correct FIFO ordering vs. a concurrent Prompt.
    for id in to_cancel {
        let _ = mgr.send(id, AgentCommand::Cancel);
    }
}

/// The console's [`AgentSpawner`] — the AppHandle-free twin of
/// [`crate::ipc::spawn::IpcSpawner`]. Delegates to the same shared spawn
/// path (`spawn_agent_shared`) so a console-spawned background agent is
/// indistinguishable from a GUI one (own workflow, manager registration,
/// loop-map tracking). Two GUI-only niceties are skipped: no
/// `PromptDispatched` event (it exists purely to render the task as the
/// first message in a UI transcript) and no `ChildFinished` emit (a
/// sidebar-only marker) — the parent completion Suggestion IS sent, by the
/// console's [`LifecycleBookkeeping`], with the same text the GUI uses.
pub struct HostSpawner {
    factory: Arc<AgentLoopFactory>,
    manager: Arc<tokio::sync::Mutex<AgentManager>>,
    agent_loops: AgentLoopMap,
}

impl HostSpawner {
    /// Create a spawner over the shared runtime pieces.
    pub fn new(
        factory: Arc<AgentLoopFactory>,
        manager: Arc<tokio::sync::Mutex<AgentManager>>,
        agent_loops: AgentLoopMap,
    ) -> Self {
        Self {
            factory,
            manager,
            agent_loops,
        }
    }
}

#[async_trait::async_trait]
impl AgentSpawner for HostSpawner {
    /// Spawn a background agent with no parent and no forced model —
    /// delegates to `spawn_with_parent` with both as `None` (mirrors
    /// `IpcSpawner`).
    async fn spawn(
        &self,
        name: &str,
        task: &str,
        role: Option<String>,
        permission: crate::agent::policy::PermissionPolicy,
    ) -> Result<AgentId, String> {
        self.spawn_with_parent(name, task, None, None, role, permission)
            .await
    }

    /// This spawner is parent-aware (tool-spawned agents record their
    /// parent, closing the completion-notification loop).
    fn parent_aware(&self) -> Option<&dyn ParentAwareSpawner> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl ParentAwareSpawner for HostSpawner {
    /// Spawn a background agent on behalf of the given parent (the agent
    /// whose `spawn_agent` tool call triggered this). Same semantics as
    /// `IpcSpawner::spawn_with_parent` minus the UI-only event emit.
    async fn spawn_with_parent(
        &self,
        name: &str,
        task: &str,
        parent_id: Option<AgentId>,
        model: Option<crate::config::ModelRef>,
        role: Option<String>,
        // §5.4 :377: the parent's policy, FORWARDED into the shared
        // spawn path (3a.5 made it real) — a free parent's
        // OverridePermissions reaches the child; the factory default
        // (ApproveViaApi) is what factory-built children carry.
        permission: crate::agent::policy::PermissionPolicy,
    ) -> Result<AgentId, String> {
        let (id, _) = spawn_agent_shared(
            SpawnHost {
                factory: self.factory.clone(),
                manager: self.manager.clone(),
                agent_loops: self.agent_loops.clone(),
            },
            name.to_string(),
            Some(task.to_string()),
            parent_id,
            SessionOptions {
                model,
                role,
                // own_plans_dir — subagents share the main dir (can't mutate)
                own_plans_dir: false,
                // no worktree binding — subagents inherit the parent's root
                worktree: None,
                permission: Some(permission),
                // subagents never resume from a transcript (3a.6)
                initial_messages: None,
                // subagents set no session limits (3a.7)
                limits: None,
                // subagents register no host tools (3c/G2)
                host_tools: vec![],
            },
        )
        .await?;
        Ok(id)
    }
}

#[async_trait::async_trait]
impl DescendantTracker for HostSpawner {
    /// Whether the agent has any running spawned descendants — delegates to
    /// the manager's `parent_id`-chain walk, same as the IPC twin.
    async fn has_running_descendants(&self, agent_id: AgentId) -> bool {
        let mgr = self.manager.lock().await;
        mgr.has_running_descendants(agent_id)
    }
}

/// The host-side agent runtime core every frontend shares: the agent
/// manager + per-agent loop map, the [`HostSpawner`] wired into the
/// factory, the fan-in event receiver, the main agent's id, and the
/// [`LifecycleBookkeeping`] — the assembly the console built inline until
/// 3a.2 (D2) hoisted it, now the piece a headless Engine, a daemon, or the
/// GUI can hold directly. Nothing here reads stdin, writes stdout, or
/// touches a Tauri handle.
///
/// Field-level mirror of the old console assembly: `AgentManager::new(256)`,
/// the fan-in receiver taken before the manager is wrapped, one
/// [`HostSpawner`] serving as both `AgentSpawner` and `DescendantTracker`
/// (set on the factory BEFORE the main spawn — the spawn tool is
/// registered per build), the main agent via [`spawn_agent_shared`] with
/// the GUI's exact args, and bookkeeping over the shared maps.
pub struct HostRuntime {
    /// The loop factory the spawner and every frontend path share.
    pub factory: Arc<AgentLoopFactory>,
    /// The agent manager (registration + fan-in) — one per runtime.
    pub manager: Arc<tokio::sync::Mutex<AgentManager>>,
    /// The per-agent loop map (steer/interrupt/approve senders).
    pub agent_loops: AgentLoopMap,
    /// The shared spawner (AgentSpawner + DescendantTracker in one).
    pub spawner: Arc<HostSpawner>,
    /// The fan-in event receiver — the runtime owns it, exactly like the
    /// GUI's event forwarder does.
    pub fanin_rx: tokio::sync::mpsc::Receiver<(AgentId, AgentEvent)>,
    /// The main agent's id (prompts are sent to it).
    pub main_id: AgentId,
    /// Lifecycle bookkeeping (running flags, parent notifications,
    /// dead-agent cleanup).
    pub bookkeeping: LifecycleBookkeeping,
}

impl HostRuntime {
    /// Build the runtime from a wired factory: create the manager, wire
    /// the [`HostSpawner`] into the factory, spawn the main agent via the
    /// same shared path as the GUI's main agent (id allocation, channel
    /// setup, loop-map insert, and registration are identical), and set
    /// up the lifecycle bookkeeping.
    pub async fn start(factory: Arc<AgentLoopFactory>) -> Result<Self, String> {
        let mut mgr = AgentManager::new(256);
        // Take the fan-in receiver before wrapping the manager (the
        // runtime owns it, like the GUI's event forwarder does).
        let fanin_rx = mgr.take_fanin_rx().expect("fan-in receiver already taken");
        let manager = Arc::new(tokio::sync::Mutex::new(mgr));
        let agent_loops: AgentLoopMap =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

        // Wire the spawn_agent tool's spawner BEFORE the main agent exists
        // (the tool is registered per build) — one HostSpawner serves as
        // both AgentSpawner and DescendantTracker.
        let spawner = Arc::new(HostSpawner::new(
            factory.clone(),
            manager.clone(),
            agent_loops.clone(),
        ));
        factory.set_spawner(spawner.clone());
        factory.set_descendant_tracker(spawner.clone());

        // The main agent — identical args to the GUI's main-agent spawn:
        // no initial prompt, no parent, no forced model, no role, and no
        // worktree binding (the main agent works the main tree).
        let (main_id, _) = spawn_agent_shared(
            SpawnHost {
                factory: factory.clone(),
                manager: manager.clone(),
                agent_loops: agent_loops.clone(),
            },
            "main".to_string(),
            None,
            None,
            SessionOptions {
                model: None,
                role: None,
                own_plans_dir: false,
                // no worktree binding — the main agent works the main tree
                worktree: None,
                // the main agent keeps the factory build's default policy
                permission: None,
                // the main agent never resumes from a transcript
                initial_messages: None,
                // the main agent sets no session limits (3a.7)
                limits: None,
                // the main agent registers no host tools (3c/G2)
                host_tools: vec![],
            },
        )
        .await
        .map_err(|e| format!("main agent spawn failed: {e}"))?;

        // The bookkeeping struct shares the manager + loop map (cloned
        // before they move into the runtime's own fields).
        let bookkeeping = LifecycleBookkeeping::new(manager.clone(), agent_loops.clone());

        Ok(Self {
            factory,
            manager,
            agent_loops,
            spawner,
            fanin_rx,
            main_id,
            bookkeeping,
        })
    }

    /// Hand the fan-in receiver to a dedicated pump (the 3a.5 engine's
    /// event pump). One-shot: the real receiver moves out and a CLOSED
    /// channel takes its place — a later recv sees `None`. The console
    /// never calls this (its REPL owns the receiver directly).
    pub fn take_fanin_rx(&mut self) -> tokio::sync::mpsc::Receiver<(AgentId, AgentEvent)> {
        let (tx, closed) = tokio::sync::mpsc::channel(1);
        drop(tx);
        std::mem::replace(&mut self.fanin_rx, closed)
    }
}

/// Console-side lifecycle bookkeeping — the console's equivalent of the GUI
/// event forwarder's manager maintenance, minus the UI emits. The REPL calls
/// one of these for EVERY agent event, exactly where the forwarder would:
///
/// - [`on_started`](Self::on_started) / [`on_turn_ended`](Self::on_turn_ended)
///   keep `AgentHandle.running` current. Those flags drive two security-
///   relevant behaviors that silently break without maintenance: the
///   workflow state-transition gate ("no plan mutations while subagents
///   run", via [`DescendantTracker`]) and
///   `cleanup_inactive_subagents` (which must never Cancel a RUNNING child).
/// - [`on_turn_ended`](Self::on_turn_ended) also sends the child's parent the
///   completion-notification `Suggestion` (once per child, with the child's
///   `last_review_report` path — the same text the GUI forwarder composes),
///   closing the orchestrator feedback loop.
/// - [`on_exited`](Self::on_exited) removes the dead agent from the manager
///   and the per-agent loop map, so both stay bounded over a long session.
pub struct LifecycleBookkeeping {
    manager: Arc<tokio::sync::Mutex<AgentManager>>,
    agent_loops: AgentLoopMap,
    /// Children already notified (first terminal event wins — a multi-turn
    /// child must not re-notify its parent).
    notified_children: std::sync::Mutex<std::collections::HashSet<AgentId>>,
}

impl LifecycleBookkeeping {
    /// Create bookkeeping over the shared manager + loop map.
    pub fn new(manager: Arc<tokio::sync::Mutex<AgentManager>>, agent_loops: AgentLoopMap) -> Self {
        Self {
            manager,
            agent_loops,
            notified_children: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// The agent started a turn — mark it running.
    pub async fn on_started(&self, id: AgentId) {
        self.manager.lock().await.set_running(id, true);
    }

    /// The agent's turn ended (a `Finished` or a final non-retrying `Error`)
    /// — mark it idle and, for a tool-spawned child, notify its parent once.
    /// `success` mirrors the GUI forwarder: `true` for `Finished`, `false`
    /// for a final error (drives the failed-reviewer protocol).
    pub async fn on_turn_ended(&self, id: AgentId, success: bool) {
        {
            let mgr = self.manager.lock().await;
            mgr.set_running(id, false);
        }
        // Notify at most once per child (mirrors the forwarder's
        // `notified_children` set — the insert decides, before the
        // parent-existence check, exactly like `events.rs`).
        if self
            .notified_children
            .lock()
            .expect("notified_children lock poisoned")
            .insert(id)
        {
            self.notify_parent(id, success).await;
        }
    }

    /// The agent's task truly terminated — mark it idle and remove it from
    /// the manager + the per-agent loop map (and from the notified set, so
    /// neither grows without bound over a long session).
    pub async fn on_exited(&self, id: AgentId) {
        {
            let mut mgr = self.manager.lock().await;
            mgr.set_running(id, false);
            mgr.remove(id);
        }
        self.agent_loops.lock().await.remove(&id);
        self.notified_children
            .lock()
            .expect("notified_children lock poisoned")
            .remove(&id);
    }

    /// Send the child's parent the completion-notification `Suggestion`,
    /// including the child's last review-report path when it wrote one — the
    /// same composition as the GUI's `notify_parent_on_completion` (minus
    /// the sidebar-only `ChildFinished` emit), including the failed-reviewer
    /// protocol: a `role: "reviewer"` child that ended (failed OR finished
    /// cleanly) with no report sets the parent's reviewer-failure latch and
    /// gets the ask-the-user text instead of the generic no-report steer.
    /// Best-effort: a full inbox or a gone parent drops the notification
    /// silently (a nicety, not a guarantee — same as the GUI).
    async fn notify_parent(&self, id: AgentId, success: bool) {
        let (parent_id, child_name, child_role) = {
            let mgr = self.manager.lock().await;
            let Some(pid) = mgr.parent_id(id) else {
                return; // no parent (the main agent / a UI-spawned agent)
            };
            let name = mgr
                .get(id)
                .map(|h| h.name.clone())
                .unwrap_or_else(|| format!("agent-{id}"));
            let role = mgr.role(id);
            (pid, name, role)
        };
        let report_path: Option<String> = self
            .agent_loops
            .lock()
            .await
            .get(&id)
            .and_then(|l| l.last_review_report());
        // Backlog 5b46674d: no !success gate — a reviewer that FINISHES
        // report-less is just as report-less as a failed one (the review did
        // not happen either way).
        let reviewer_failed_no_report =
            child_role.as_deref() == Some("reviewer") && report_path.is_none();
        if reviewer_failed_no_report {
            if let Some(parent_loop) = {
                let loops = self.agent_loops.lock().await;
                loops.get(&parent_id).cloned()
            } {
                parent_loop.set_reviewer_failure_pending(true);
            }
        }
        let text = if reviewer_failed_no_report {
            crate::runtime::turn_resolve::reviewer_failure_suggestion_text(&child_name)
        } else {
            let outcome = if success { "finished" } else { "failed" };
            crate::runtime::turn_resolve::completion_suggestion_text(
                &child_name,
                outcome,
                report_path.as_deref(),
            )
        };
        let mgr = self.manager.lock().await;
        let _ = mgr.send(parent_id, AgentCommand::Suggestion(text.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentLoop;
    use crate::tool::ToolRegistry;
    use crate::app::workflow::Workflow;

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
    /// tool set (read_files + file_write + git + git_read +
    /// write_review_report + create_plan + complete_step + web_fetch +
    /// memory_recall), and insert it into an `agent_loops` map under
    /// `parent_id`. Returns the map + the loop's workflow handle so the test
    /// can drive state transitions.
    async fn parent_in_state(
        parent_id: AgentId,
        state: crate::app::workflow::WorkflowState,
    ) -> (
        Arc<tokio::sync::Mutex<std::collections::HashMap<AgentId, Arc<AgentLoop>>>>,
        Arc<tokio::sync::Mutex<Workflow>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(crate::tool::agent::sandbox::Sandbox::new(dir.path()).unwrap());
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        // Drive the workflow into the requested state.
        {
            let mut wf = workflow.lock().await;
            match state {
                crate::app::workflow::WorkflowState::Planning => {} // default
                crate::app::workflow::WorkflowState::Executing => {
                    wf.create_plan("p", "g", "c", vec!["s".into()]).unwrap();
                }
                crate::app::workflow::WorkflowState::Complete => {
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
        registry.register(Box::new(
            crate::tool::agent::read_files::ReadFilesTool::new((*sandbox).clone()),
        ));
        registry.register(Box::new(
            crate::tool::agent::file_write::FileWriteTool::new((*sandbox).clone()),
        ));
        registry.register(Box::new(crate::tool::agent::git::GitTool::new(dir.path())));
        registry.register(Box::new(
            crate::tool::agent::git_read_tool::GitReadTool::new(dir.path()),
        ));
        registry.register(Box::new(
            crate::tool::agent::write_review_report::WriteReviewReportTool::new(
                dir.path().join("reviews"),
            ),
        ));
        registry.register(Box::new(
            crate::app::tool::workflow::plan::CreatePlanTool::new(workflow.clone()),
        ));
        registry.register(Box::new(
            crate::app::tool::workflow::plan::CompleteStepTool::new(workflow.clone()),
        ));
        // Read-only queries the reviewer's base list names — registered so the
        // intersection keeps them (the production main agent registers these).
        registry.register(Box::new(crate::tool::agent::web_fetch::WebFetchTool::new()));
        let store: Arc<dyn crate::memory::MemoryStoreTrait> = Arc::new(
            crate::memory::MemoryStore::open_in_memory(Arc::new(
                crate::memory::embedder::HashEmbedder::new(),
            ))
            .unwrap(),
        );
        registry.register(Box::new(
            crate::tool::memory::retrieval::MemorySearchTool::new(store),
        ));
        // Code-graph read tools (the reviewer's base list names all four).
        let graph = Arc::new(
            crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap(),
        );
        registry.register(Box::new(
            crate::tool::agent::codegraph::GraphSearchTool::new(graph.clone()),
        ));
        registry.register(Box::new(
            crate::tool::agent::codegraph::GraphContextTool::new(graph.clone()),
        ));
        registry.register(Box::new(
            crate::tool::agent::codegraph::GraphImpactTool::new(graph.clone()),
        ));
        registry.register(Box::new(crate::tool::agent::codegraph::GraphPathTool::new(
            graph,
        )));
        // The read-only backlog query (the reviewer's base list names it).
        registry.register(Box::new(
            crate::app::tool::workflow::backlog::BacklogListTool::new(Arc::new(
                tokio::sync::Mutex::new(crate::app::backlog::BacklogStore::open(
                    dir.path().join("backlog.jsonl"),
                )),
            )),
        ));
        let provider: Arc<dyn crate::provider::LlmClient> = Arc::new(NoopProvider);
        let agent = AgentLoop::new(
            crate::agent::AgentLoopConfig {
                provider,
                tools: Arc::new(registry),
                workflow: crate::app::workflow::as_handle(workflow.clone()),
                sandbox,
                safety_mode: crate::config::SafetyMode::Autonomous,
                context_manager: crate::agent::context::ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
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
        let spec = crate::agent::factory::AgentRootSpec {
            sandbox: crate::tool::agent::sandbox::Sandbox::new(&worktree_root).unwrap(),
            project_root: worktree_root.clone(),
            codegraph: None,
        };
        // Build a parent loop the way build_with_root_spec does: the spec
        // stamped on the loop + the worktree plans dir.
        let parent = AgentLoop::new(
            crate::agent::AgentLoopConfig {
                provider: Arc::new(NoopProvider),
                tools: Arc::new(ToolRegistry::new()),
                workflow: crate::app::workflow::as_handle(Arc::new(
                    tokio::sync::Mutex::new(Workflow::new(
                        plans_dir.clone(),
                    )),
                )),
                sandbox: Arc::new(
                    crate::tool::agent::sandbox::Sandbox::new(dir.path()).unwrap(),
                ),
                safety_mode: crate::config::SafetyMode::Autonomous,
                context_manager: crate::agent::context::ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
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
            crate::agent::AgentLoopConfig {
                provider: Arc::new(NoopProvider),
                tools: Arc::new(ToolRegistry::new()),
                workflow: crate::app::workflow::as_handle(Arc::new(
                    tokio::sync::Mutex::new(Workflow::new(
                        dir.path().join("plans"),
                    )),
                )),
                sandbox: Arc::new(
                    crate::tool::agent::sandbox::Sandbox::new(dir.path()).unwrap(),
                ),
                safety_mode: crate::config::SafetyMode::Autonomous,
                context_manager: crate::agent::context::ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
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
    impl crate::provider::LlmClient for NoopProvider {
        fn capabilities(&self) -> &crate::provider::Capabilities {
            // A static openai caps — never read in these tests.
            use std::sync::OnceLock;
            static CAPS: OnceLock<crate::provider::Capabilities> = OnceLock::new();
            CAPS.get_or_init(crate::provider::Capabilities::openai)
        }
        fn kind(&self) -> crate::provider::ProviderKind {
            crate::provider::ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "noop"
        }
        async fn complete(
            &self,
            _: &[crate::provider::Message],
            _: &[crate::provider::ToolSchema],
            _: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>>
        {
            Err(crate::error::Error::Provider(
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
        let (agent_loops, _wf) =
            parent_in_state(1, crate::app::workflow::WorkflowState::Planning).await;
        let list = compute_subagent_allowlist(&agent_loops, Some(1), &Some("reviewer".into()))
            .await
            .expect("reviewer spawn with a parent returns a list");

        // Read tools the parent allows (Planning allows AutoRun Agent tools).
        assert!(list.contains(&"read_files".to_string()), "read_files allowed");
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
            parent_in_state(2, crate::app::workflow::WorkflowState::Executing).await;
        let list = compute_subagent_allowlist(&agent_loops, Some(2), &Some("reviewer".into()))
            .await
            .expect("reviewer spawn with a parent returns a list");

        assert!(list.contains(&"read_files".to_string()));
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
        // absent (Planning hides them) but read_files + create_plan are
        // present. This is the core "subagent ⊆ parent" property.
        let (agent_loops, _wf) =
            parent_in_state(3, crate::app::workflow::WorkflowState::Planning).await;
        let list = compute_subagent_allowlist(&agent_loops, Some(3), &None)
            .await
            .expect("unrestricted spawn with a parent returns a list");

        assert!(
            list.contains(&"read_files".to_string()),
            "parent allows read_files"
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
