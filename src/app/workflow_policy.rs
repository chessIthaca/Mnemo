// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The workflow agent's policy: wraps today's `Workflow` so every kernel
//! coupling consults this wrapper (master-spec :200-202). Job 2b moves the
//! implementation under `mnemo::app`.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::agent::policy::{
    AgentPolicy, Continuation, GateVerdict, KernelPromptBlocks, PolicyBuildCtx, PromptHook,
    RoleSpec, RouteKey, SpendTags, ToolSurface,
};
use crate::app::policy_factory::TeamLeadWiring;
use crate::app::workflow::{Workflow, WorkflowState};
use crate::project::{Constitution, ConstitutionScope};
use crate::provider::Capabilities;
use crate::tool::{ToolRegistry, ToolResult};

const MNEMO2_TEAM_APPEND: &str = concat!(
    "\n\nMNEMO2 TEAM RULES:\n",
    "- Generic spawn_agent is unavailable. Delegate implementation through team_dispatch. For each logical task, choose a unique worker_id of 8-64 ASCII letters/digits and retain the returned record. After a timeout or uncertain host result, retry with the SAME worker_id, brief, and model, or inspect it with team_status; never guess a replacement ID or create a second worker for that task.\n",
    "- TeamManager records and team_status are the ONLY worker lifecycle and ID authority. Never infer worker status from Git branches, worktree timestamps, files, or directories, and never read or edit team.json. Use Git only to review or implement source changes; TeamManager owns worker state, worktrees, branches, and merges.\n",
    "- Workers do the majority of the work. You plan, split the work into independent tasks with disjoint file ownership, brief, review, and merge; dispatch every self-contained implementation task (code, tests, verification, commit) to a worker rather than writing it yourself, and run independent tasks in parallel up to the slot limit. Implement directly only trivial glue or a one-line fix.\n",
    "- Workers are full executors: file_edit, file_write, shell, git, and tests in their own worktree, with no token or cost budget. Never brief a worker as read-only, content-only, or 'return the file text to me'. If a worker reports missing tools or a host error, surface it to the user as a defect instead of degrading the task or taking the work back.\n",
    "- Choose each worker's model and reasoning effort deliberately: pass team_dispatch's `model` (an exact spec from its enum) and `reasoning_effort` matched to the task — a strong model at high effort for design-heavy, cross-cutting, or debugging work; a fast, inexpensive model at low effort for mechanical edits, boilerplate, or test scaffolding. Omitting them uses the configured defaults.\n",
    "- Workers implement in their assigned worktrees while you review on the main lead thread. Coordinate with team_message using exact returned IDs. A blocked-worker notice requires an actual status check and a reply, team_stop, or team_compact as appropriate; never edit TeamManager's backing state.\n",
    "- There is no cumulative worker token or cost budget. Prompt, completion, cached, and context tokens are telemetry, never a reason to pause a worker, invent a limit, or request a budget increase. Operational slot/session failures and provider context limits are reported explicitly and are not spending budgets.\n",
    "- A valid call returning a domain error is not malformed JSON. Stop repeating the same call: correct the named precondition or inspect TeamManager state. Host/domain errors never justify editing the ledger or probing worker files for status.\n",
    "- Record reviews with team_review and only checks you actually ran with team_check. Close findings with evidence or a written wontfix justification. Every fix commit requires affected checks and a fresh delta review. Use review_fix only for one-file, non-test source edits; route larger findings with team_route_fix.\n",
    "- Call team_merge only after exact-tip checks pass and every finding closes. TeamManager performs the app-level merge into the non-main lead branch; never call git merge or git push yourself. team_revert_merge is available only before landing.\n",
);

/// TeamLead workflow map — replaces `WORKFLOW_LIFECYCLE` for the Mnemo2 lead
/// (`.coding/design/mnemo2.md` Phase 6). Kept lean: the head costs tokens once
/// per session.
const TEAM_WORKFLOW_LIFECYCLE: &str = "\
WORKFLOW — a plan board. You hold any number of plans at once and run them in \
parallel through workers; no workflow state locks your tools.

- PLAN FIRST. Any real work — anything you would delegate — starts with \
plan_create. A plan is the record of intent: put the why, the constraints, and \
the decisions already taken in `context`. Workers receive the plan \
automatically and a later session resumes from it. Plan ALL the work before \
you dispatch; explore with read tools first. Tiny direct edits by you (a \
one-line fix, a typo) need no plan.
- DELEGATE each step with team_dispatch(plan_id, step, ...). The worker \
already gets the plan, its step's full text, and the step list, so your brief \
carries only the task specifics (files, commands, acceptance). Pick the worker \
model with `tier`: choose the cheapest tier that fits the step. Run independent \
steps and plans in parallel, then END the turn — TeamManager notices wake you. \nNever end a turn on a bare tool call: finish with one line saying what you just \ndid and which workers you are waiting on, and do any work that needs no \nworker output (merging a finished worker, the next independent dispatch) first.
- RECORD decisions with plan_update as you settle them (decision, \
context_append, add_steps). Append; never rewrite what is recorded. Context is \
stable intent that every worker reads; record execution state (commits, review \
status, what is next) with `progress`, never in context.
- CONTRACTS: before dispatching steps that touch the same file or module \
boundary, write the contract (exports, signatures, data shapes, file ownership) \
with plan_update `interface`; dispatch refuses shared files without one. \
Dispatching a step while another runs adds a final \"Integration check\" step \
that must be done before the plan closes.
- STEPS TICK ON MERGE: a step is checked off once its tasks merged and none is \
open. Act on notices: review (team_review, team_check), then team_merge. Tick \
a step you did yourself with plan_step_done.
- PLANS CLOSE AUTOMATICALLY when every step is done, every task is merged or \
cancelled, and every finding is closed: a review report lands in \
.coding/reviews/ and the plan is digested into memory. A notice says what \
blocks a close. plan_abandon closes a plan without review (last resort). The \
plan file stays in .coding/plans/ either way.

PLAN KIND (set at plan_create): \"implementation\" (default); \"research\" for \
work that changes no source code. Bugs: ALWAYS kind:\"bug_fixing\" with `bug` \
(the symptom); to close, record the worker's regression test with plan_update \
regression_test (it must exist in the code graph). Bug-triggered FEATURES (new \
capabilities, new dependencies, several modules) are kind:\"implementation\" \
with the bug documented as motivation.
";

/// TeamLead closing sequence — replaces the classic one (which spawns a
/// reviewer) after the universal part of `APP_RULES`. The lead is the reviewer
/// of record through team_review.
const TEAM_CLOSING_SEQUENCE: &str = "\
CLOSING SEQUENCE — you are the reviewer of record through team_review:
1. Merge — every task of the plan is merged (team_merge) or cancelled. \
Findings close with evidence or a written wontfix justification.
2. Test — run the project's checks on the lead branch; route larger fixes to a \
worker. Bug plans: the worker's regression test exercises the changed path and \
is recorded with plan_update.
3. Commit your own changes (including .coding/plans) to the current working \
branch.
The plan then closes by itself with its review report — there is no finish call.
";

/// The marker `k.rules` carries between its universal part and the classic
/// closing sequence. The TeamLead head splits there; a test pins it.
const RULES_CLOSING_MARKER: &str = "CLOSING SEQUENCE";

/// The tools the TeamLead mode hides (unregistered and refused): generic
/// `spawn_agent`, and the classic single-plan tools the plan board replaces.
pub const TEAM_HIDDEN_TOOLS: [&str; 7] = [
    "spawn_agent",
    "create_plan",
    "update_plan",
    "complete_step",
    "finish",
    "abandon_plan",
    "current_plan",
];

/// The strategy line the TeamLead head drops (generic `spawn_agent`).
const STRATEGY_SPAWN_LINE: &str = "- spawn_agent:";

/// Which workflow this policy runs (`.coding/design/mnemo2.md` Phase 5.1).
#[derive(Clone)]
enum WorkflowMode {
    /// The kernel's Classic workflow: the plan workflow with a
    /// reviewer-written review report.
    Classic,
    /// The Mnemo2 lead: plans live on the plan board, tasks run on
    /// TeamManager workers, and the tool surface is free.
    TeamLead(TeamLeadWiring),
}

/// The workflow agent's policy — pre-2a direct-read behavior, with scoped
/// Mnemo2 tool vetoes and team guidance.
pub struct WorkflowPolicy {
    pub(crate) workflow: Arc<Mutex<Workflow>>,
    /// The owning agent loop, attached by the factory once the loop is
    /// final (Arc-wrapped) — 2a.6. Facts the Workflow doesn't carry (today:
    /// the subagent flag) read through this back-ref; an empty Weak (before
    /// the attach, or after the loop is dropped) reads as not-a-subagent,
    /// matching the flag's default.
    loop_ref: std::sync::OnceLock<std::sync::Weak<crate::agent::AgentLoop>>,
    /// Tool names this policy must refuse outright (Mnemo2 blocks
    /// `spawn_agent` so the lead cannot create untracked workers).
    blocked_tools: Vec<String>,
    /// Classic (default) or TeamLead.
    mode: WorkflowMode,
}

impl WorkflowPolicy {
    /// Wrap the loop's workflow (factory build path, 2a.2). The factory
    /// also calls [`WorkflowPolicy::attach_loop`] once the loop is Arc'd
    /// (2a.6).
    pub fn new(workflow: Arc<Mutex<Workflow>>) -> Self {
        Self {
            workflow,
            loop_ref: std::sync::OnceLock::new(),
            blocked_tools: Vec::new(),
            mode: WorkflowMode::Classic,
        }
    }

    /// Run the Mnemo2 TeamLead workflow: the plan-board head text and tail, a
    /// free tool surface, and turn-end Stop. `spawn_agent` and the classic
    /// plan tools are refused.
    pub fn with_team_lead(mut self, wiring: TeamLeadWiring) -> Self {
        for hidden in TEAM_HIDDEN_TOOLS {
            if !self.blocked_tools.iter().any(|name| name == hidden) {
                self.blocked_tools.push(hidden.to_string());
            }
        }
        self.mode = WorkflowMode::TeamLead(wiring);
        self
    }

    /// Add outright tool vetoes (Mnemo2 uses this to block `spawn_agent` so
    /// the lead can only delegate through the tracked team runtime).
    pub fn with_blocked_tools(mut self, blocked: Vec<String>) -> Self {
        self.blocked_tools = blocked;
        self
    }
    /// Attach the owning agent loop (2a.6). Idempotent: the first attach
    /// wins.
    pub fn attach_loop(&self, loop_weak: std::sync::Weak<crate::agent::AgentLoop>) {
        let _ = self.loop_ref.set(loop_weak);
    }
    /// The loop back-ref (2a.6): `None` until attached or after the loop
    /// is dropped — read via `upgrade()`.
    pub fn loop_ref(&self) -> Option<std::sync::Weak<crate::agent::AgentLoop>> {
        self.loop_ref.get().cloned()
    }
    /// The workflow handle (kernel helpers occasionally need the state map
    /// during migration; shrinks as sites flip).
    pub fn workflow(&self) -> Arc<Mutex<Workflow>> {
        Arc::clone(&self.workflow)
    }
}

/// Compose the full system prompt (back-compat wrapper, moved app-side in
/// 2b.3): the stable head + the volatile tail over this workflow's section.
/// Production composes via the policy (2a.4: the constitution holder's
/// `stable_head` + `build_volatile_tail`); this wrapper serves the tests and
/// any back-compat caller.
pub fn build_system_prompt(
    caps: &crate::provider::Capabilities,
    workflow: &Workflow,
    constitution: &crate::project::Constitution,
    memories: Option<&str>,
) -> String {
    let mut prompt = crate::agent::prompt::build_stable_head(constitution);
    prompt.push_str(&crate::agent::prompt::build_volatile_tail(
        caps,
        &workflow_section(workflow),
        memories,
    ));
    prompt
}

/// The turn's routing key: today's ordered slot chain, moved app-side in
/// 2b.3 (the kernel's compat constructor retired with the route consult;
/// `WorkflowPolicy::route` is its sole production caller — the routing-table
/// golden pins the parity). The order mirrors `ConfigModelResolver::resolve`'s
/// pre-2a.6 arm order: the skill override, the subagent override, the
/// bug-fixing plan-kind override, then the state slot. Reviewing resolves
/// `executing` (`[models.reviewing]` is reviewer-spawn-only); Skill and
/// Subagent carry NO state slot — their own arms or the default cover them.
pub fn build_route_key(
    workflow_state: crate::app::workflow::WorkflowState,
    skill_name: Option<&str>,
    plan_kind: Option<crate::app::workflow::PlanKind>,
    is_subagent: bool,
) -> crate::agent::policy::RouteKey {
    use crate::app::workflow::{PlanKind, WorkflowState};
    let mut slots = Vec::new();
    if let Some(name) = skill_name {
        slots.push(format!("skill:{name}"));
    }
    if is_subagent {
        slots.push("subagent".to_string());
    }
    if plan_kind == Some(PlanKind::BugFixing)
        && matches!(
            workflow_state,
            WorkflowState::Executing | WorkflowState::Reviewing
        )
    {
        slots.push("bug_fixing".to_string());
    }
    match workflow_state {
        WorkflowState::Planning => slots.push("planning".to_string()),
        WorkflowState::Executing | WorkflowState::Reviewing => {
            slots.push("executing".to_string());
        }
        WorkflowState::Complete => slots.push("complete".to_string()),
        WorkflowState::Skill | WorkflowState::Subagent => {}
    }
    crate::agent::policy::RouteKey { slots }
}

#[async_trait]
impl AgentPolicy for WorkflowPolicy {
    fn stable_head(&self, k: &KernelPromptBlocks, c: &Constitution) -> String {
        if matches!(self.mode, WorkflowMode::TeamLead(_)) {
            return team_stable_head(k, c);
        }
        let mut head =
            crate::agent::policy::assemble_stable_head(k, &c.scoped(ConstitutionScope::Classic));
        if self.blocked_tools.iter().any(|name| name == "spawn_agent") {
            head.push_str(MNEMO2_TEAM_APPEND);
        }
        head
    }
    fn register_tools(&self, _reg: &mut ToolRegistry, _ctx: &PolicyBuildCtx) {
        // D5: registration stays kernel-called in 2a (factory fns run as today).
    }
    async fn volatile_tail(&self, _caps: &Capabilities) -> String {
        // TeamLead: the compact board summary — the tail is re-processed
        // every turn, so it stays short.
        if let WorkflowMode::TeamLead(wiring) = &self.mode {
            return wiring.board.tail_summary();
        }
        // 2a.4: the real delegate — lock the workflow once, assemble the
        // same bytes the kernel's workflow_section produced pre-2a.4.
        let wf = self.workflow.lock().await;
        workflow_section(&wf)
    }
    async fn tool_surface(&self) -> ToolSurface {
        // TeamLead: no workflow state gates the lead. The hidden tools are
        // unregistered and refused in `gate_call`.
        if matches!(self.mode, WorkflowMode::TeamLead(_)) {
            return ToolSurface {
                advertise: Some(crate::tool::ToolFilter::Free),
                gate: Some(crate::tool::ToolFilter::Free),
            };
        }
        // 2a.3: the real delegate — one lock, both filters, exactly today's
        // reads. advertise is the plan-frozen schema filter (byte-stable
        // while a plan is active — turn.rs :553-564's advertisement); gate
        // is the per-state enforcement filter (dispatch.rs :253).
        let wf = self.workflow.lock().await;
        ToolSurface {
            advertise: Some(wf.schema_filter()),
            gate: Some(wf.allowed_tools()),
        }
    }
    async fn gate_call(&self, name: &str, _args: &Value) -> GateVerdict {
        if self.blocked_tools.iter().any(|blocked| blocked == name) {
            let team = matches!(self.mode, WorkflowMode::TeamLead(_));
            let message = if team && name != "spawn_agent" && TEAM_HIDDEN_TOOLS.contains(&name) {
                format!(
                    "'{name}' is not available to the lead: plans live on the board — use \
                     plan_create, plan_update, plan_show, plan_list, plan_step_done, \
                     plan_abandon"
                )
            } else {
                format!(
                    "'{name}' is disabled in Mnemo2; use team_dispatch to delegate a tracked worker"
                )
            };
            return GateVerdict::Deny { message };
        }
        // 2a: Allow — the surface filter above does all of today's
        // enforcement work; the plan-mutation gate and the research-write
        // verdict keep their tool-side homes in dispatch.rs until 2b/2c
        // (master-spec :215-216).
        GateVerdict::Allow
    }
    async fn route(&self) -> RouteKey {
        // 2a.6: the policy emits today's slot chain — the same builder the
        // kernel's compat constructor uses
        // (crate::model_resolver::build_route_key), sourcing: the workflow
        // state + the active plan's kind from its own workflow handle, the
        // skill name from the workflow's active skill overlay, and the
        // subagent flag from the loop back-ref (an unattached or dropped
        // loop reads as not-a-subagent — the flag's default). NoPolicy's
        // route() returns ["free"]; this emit keeps production loops on
        // the state/subagent chain (the routing golden pins the outcome
        // table); the kernel consult flip lands with the 2b/2c engine seam.
        // TeamLead: one `lead` slot, not a per-state slot — the lead's model
        // must not change between workflow states. The resolver skips a slot
        // it has no config entry for and falls back to the default model.
        if matches!(self.mode, WorkflowMode::TeamLead(_)) {
            return RouteKey {
                slots: vec!["lead".to_string()],
            };
        }
        let route_key = {
            let wf = self.workflow.lock().await;
            let is_subagent = self
                .loop_ref()
                .and_then(|weak| weak.upgrade())
                .map(|loop_| loop_.is_subagent())
                .unwrap_or(false);
            build_route_key(
                wf.state(),
                wf.active_skill().map(|s| s.name.as_str()),
                wf.active_plan_kind(),
                is_subagent,
            )
        };
        route_key
    }
    async fn on_prompt(&self, text: &str) -> PromptHook {
        // 2a.5: the unattended marker read moves here (the runtime arms
        // consult this hook with None = the runtime's own derivation;
        // Some SETS the flag outright). tail_rewrite stays None in 2a —
        // the plan nudge is kernel-side same-turn (turn.rs :3524); it
        // rides this hook in later phases.
        let unattended = Some(text.starts_with(crate::runtime::channels::UNATTENDED_PROMPT_PREFIX));
        PromptHook {
            unattended,
            tail_rewrite: None,
        }
    }
    async fn after_tool(&self, _name: &str, _result: &ToolResult) {}
    async fn on_turn_end(&self, t: &crate::agent::policy::TurnSummary) -> Continuation {
        // 2a.5: the state predicate moves here — workflow_expects_progress's
        // logic verbatim (loop_impl :2442-2445; the fn stays for kernel
        // diagnostics). note is None: the gate's own streak mechanics push
        // the synthetic-continue message kernel-side.
        // TeamLead: worker notices and the user drive the lead, so a turn
        // always ends.
        if matches!(self.mode, WorkflowMode::TeamLead(_)) {
            return Continuation::Stop;
        }
        let wf = self.workflow.lock().await;
        let state = wf.state();
        if matches!(state, WorkflowState::Executing | WorkflowState::Reviewing)
            || (t.unattended && state == WorkflowState::Planning)
        {
            Continuation::Continue { note: None }
        } else {
            Continuation::Stop
        }
    }
    async fn spend_tags(&self) -> SpendTags {
        // 2a.8: the plan/step tags read off the workflow — the same two
        // fields turn.rs::read_plan_scope assembled (the active plan id +
        // the current step's index); the spend-row shape is unchanged, and
        // the kernel's empty-tag fallback keeps NoPolicy-era loops on the
        // kernel's own read.
        let wf = self.workflow.lock().await;
        SpendTags {
            plan_id: wf.plan_id().map(str::to_string),
            step_index: wf.current_step().map(|s| s.index),
        }
    }
    fn spawn_roles(&self) -> &[RoleSpec] {
        &[]
    }
    async fn on_session_end(&self) {
        // 2a.8: the hook exists for the seam (master-spec :230) and the
        // runtime's spawn_consolidation consults it before the memory
        // session teardown. The consolidation itself stays kernel-side in
        // 2a (end_session + consolidate_session in the runtime task); the
        // Workflow carries nothing session-shaped to flush at end, so the
        // delegate is a documented no-op — the 2b/2c engine seam owns real
        // session-end work.
    }
}

/// The TeamLead stable head: `assemble_stable_head`'s block layout with the
/// lifecycle swapped for `TEAM_WORKFLOW_LIFECYCLE`, the closing sequence swapped
/// for `TEAM_CLOSING_SEQUENCE`, the `spawn_agent` strategy line dropped, the
/// constitution scoped to Team, and the Mnemo2 team rules appended. The
/// universal rules and strategy are derived from the kernel blocks by string
/// surgery (so the kernel's text stays the single source); a missing marker
/// keeps the block whole, and a test pins the markers.
fn team_stable_head(k: &KernelPromptBlocks, c: &Constitution) -> String {
    let universal_rules = match k.rules.find(RULES_CLOSING_MARKER) {
        Some(at) => &k.rules[..at],
        None => k.rules,
    };
    let rules = format!("{}\n\n{TEAM_CLOSING_SEQUENCE}", universal_rules.trim_end());
    let strategy: String = k
        .strategy
        .split_inclusive('\n')
        .filter(|line| !line.starts_with(STRATEGY_SPAWN_LINE))
        .collect();
    let mut out = String::new();
    for block in [
        k.preamble,
        TEAM_WORKFLOW_LIFECYCLE,
        rules.as_str(),
        k.discipline,
        strategy.as_str(),
        k.memory_records,
    ] {
        if !block.trim().is_empty() {
            out.push_str(block);
            out.push_str("\n\n");
        }
    }
    crate::agent::policy::push_constitution_sections(
        &mut out,
        &c.scoped(ConstitutionScope::Team),
    );
    out.push_str(MNEMO2_TEAM_APPEND);
    out
}

// ---------------------------------------------------------------------------
// The volatile tail's per-state guidance — moved verbatim from prompt.rs in
// 2a.4 (step file :636-638: the workflow-dependent half moves into this
// module; the kernel keeps the recall block + CAPABILITIES + ordering).
// ---------------------------------------------------------------------------

/// The default static guidance for the PLANNING state of the volatile tail's
/// workflow section.
const STATE_PLANNING: &str = "\
PLANNING: read tools only. Explore, then create_plan your steps. Write tools \
unlock only when a plan exists — no project changes without one.
";

/// The static guidance shown under the current step in EXECUTING; the dynamic
/// CURRENT PLAN / GOAL / PROGRESS / CURRENT STEP lines are interpolated from
/// live workflow state.
const STATE_EXECUTING: &str = "Complete the current step, then complete_step. Stay in scope.\n";

/// The static guidance for the REVIEWING state — the closing-sequence
/// driver.
///
/// Deliberately a POINTER, not a restatement: this block lives in the volatile
/// tail, which sits after the cached stable head and is re-processed on every
/// turn. The verdict rule, the bug checklist and the reviewer-only authorship
/// rule are stated once, in `APP_RULES` (cached). Keeping a second copy here
/// cost ~200 tokens per Reviewing turn and bought nothing — the head is in
/// context either way. Enforced by
/// `reviewing_state_block_points_at_app_rules_without_duplicating_them`.
const STATE_REVIEWING: &str = "\
All steps complete — REVIEWING: run the closing sequence (APP RULES) — \
spawn_agent role:\"reviewer\" (omit the model param — the configured \
reviewing model is authoritative) over ALL uncommitted changes, then fix EVERY \
finding yourself (the write tools are available to YOU, not the reviewer), \
re-run the tests, commit, and finish(report path). No complete_step / \
create_plan here — the plan is done; update_plan IS available (fix-findings \
scope changes and appended follow-on steps — the workflow accepts full edits \
in Reviewing; complete_step stays hidden, so appended steps can't be checked \
off mid-review). backlog_add / backlog_status are available too — a user \
request to queue or update a backlog item is never blocked mid-review.
";

/// The static guidance for the COMPLETE state.
///
/// Directive (user request 2026-09-06: "more eager switching to plan"): tells
/// the agent WHEN to plan (code-change task) vs. when to answer freeform (pure
/// question), so it doesn't explain how it *would* fix something instead of
/// actually planning + executing it. The code-level nudge — the kernel's
/// `append_plan_nudge` (prompt.rs; consult-side in 2a.5) — reinforces this
/// when a work verb is detected in the user's latest message.
const STATE_COMPLETE: &str = "\
All steps complete. If the user's message implies code changes (fix, add, \
implement, refactor, update + a code reference), treat it as a NEW TASK: \
explore with read tools, then call create_plan — do NOT answer freeform. \
Only answer freeform for pure questions (how/where/what). Skills (e.g. \
merge_to_main) may also start here.
";

/// The static guidance for the SUBAGENT state — a parented sub-agent's role
/// state (2026-01-03): a single-task worker, not a plan-lifecycle owner. The
/// plan context above it is the parent's, shown read-only.
const STATE_SUBAGENT: &str = "\
You are a background sub-agent: a single-task worker spawned by a parent. \
Work the task in your spawn prompt to completion and put your findings in \
your final answer (the parent reads it). The plan context above is your \
parent's — read-only orientation, not a work order: you cannot create or \
advance plans, and your tools are exactly your spawn-time allow-list. When \
done, stop — no plan lifecycle applies to you.
";

/// Truncate a step's display text to a short punchline for the volatile tail.
///
/// The tail is the only part re-processed per `complete_step` (the R5
/// byte-stable footer keeps everything before it cached), so the CURRENT STEP
/// line must stay short: the bold header when present, else the first 120
/// chars. Char-safe (`chars().take` never splits a UTF-8 sequence).
fn short_step_text(text: &str) -> String {
    const MAX: usize = 120;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        let head: String = text.chars().take(MAX).collect();
        format!("{head}…")
    }
}

/// One-line, budget-truncated form of a plan's Bug/Context for the
/// volatile tail (backlog 77ff8f45): a fresh session picking up an
/// in-flight plan gets the symptom + the context gist in the footer —
/// the tail is its only plan-derived injection. The full detail,
/// including a bug plan's `## Detailed steps`, stays in the plan file
/// (the crash-resumption document). Whitespace-collapsed to one line and
/// char-safe (`chars().take` never splits a UTF-8 sequence).
fn short_context_line(text: &str, budget: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= budget {
        collapsed
    } else {
        let head: String = collapsed.chars().take(budget).collect();
        format!("{head}…")
    }
}

/// Push one state's static guidance, skipping empty sections and
/// normalizing the trailing newline — byte-identical to the compiled consts,
/// which each end with exactly one `\n`.
fn push_state_text(out: &mut String, text: &str) {
    let t = text.trim_end();
    if !t.is_empty() {
        out.push_str(t);
        out.push('\n');
    }
}

/// Build the workflow section of the system prompt — the app-half the
/// policy's `volatile_tail` assembles (moved from prompt.rs in 2a.4).
///
/// The static per-state guidance comes from the compiled STATE_* consts; the
/// dynamic lines — CURRENT PLAN / GOAL / PROGRESS / CURRENT STEP, the skill
/// name/goal, the sub-agent plan context — are interpolated from live
/// workflow state. The CAPABILITIES block stays kernel-side (prompt.rs's
/// `build_volatile_tail`) in 2a.4, so it does not ride here.
pub(crate) fn workflow_section(workflow: &Workflow) -> String {
    let state = workflow.state();
    let mut s = String::new();

    s.push_str("# WORKFLOW STATE\n\n");
    s.push_str(&format!("Current state: {state}\n\n"));

    match state {
        WorkflowState::Planning => {
            push_state_text(&mut s, STATE_PLANNING);
        }
        WorkflowState::Executing => {
            if let Some(plan) = workflow.plan() {
                // Show the nesting breadcrumb when this is a sub-plan, so the
                // agent knows there's a parent to fall back to.
                let parents = workflow.parent_titles();
                if parents.is_empty() {
                    s.push_str(&format!("CURRENT PLAN: {}\n", plan.title));
                } else {
                    s.push_str(&format!(
                        "CURRENT PLAN (sub-plan, depth {}): {} ▸ {}\n",
                        workflow.plan_depth(),
                        parents.join(" ▸ "),
                        plan.title
                    ));
                }
                s.push_str(&format!("GOAL: {}\n", plan.goal));
                // Backlog 77ff8f45: surface the Bug + Context in the
                // resume footer — a fresh session picking up an
                // in-flight plan gets the symptom + the context gist
                // here; the full detail, including a bug plan's
                // Detailed steps, stays in the plan file (the
                // crash-resumption document). One line each — the tail
                // is re-processed per step bump, so every token is paid
                // on every bump.
                if let Some(bug) = &plan.bug_symptom {
                    s.push_str(&format!("BUG: {}\n", short_context_line(bug, 160)));
                }
                if !plan.context.trim().is_empty() {
                    s.push_str(&format!(
                        "CONTEXT: {}\n",
                        short_context_line(plan.context.trim(), 240)
                    ));
                }
                s.push_str(&format!(
                    "PROGRESS: {}/{} steps complete\n",
                    plan.completed_count(),
                    plan.steps.len()
                ));
                if let Some(step) = plan.current_step() {
                    // Show the step HEADER (the bold punchline) when present,
                    // else a truncated body. The full step text stays
                    // available in the conversation (the create_plan /
                    // update_plan results). Keeping this line short matters:
                    // with the R5 CONTEXT_FOOTER the volatile tail is the
                    // only part re-processed per complete_step, so every
                    // token here is paid on every step bump.
                    let current = step
                        .header
                        .as_deref()
                        .map(short_step_text)
                        .unwrap_or_else(|| short_step_text(&step.text));
                    s.push_str(&format!("CURRENT STEP: \"{current}\" ← resume here\n\n",));
                    push_state_text(&mut s, STATE_EXECUTING);
                }
            }
        }
        WorkflowState::Reviewing => {
            push_state_text(&mut s, STATE_REVIEWING);
        }
        WorkflowState::Complete => {
            push_state_text(&mut s, STATE_COMPLETE);
        }
        WorkflowState::Skill => {
            if let Some(skill) = workflow.active_skill() {
                s.push_str(&format!(
                    "You are running the skill \"{name}\". Drive toward this goal:\n\n{prompt}\n\n",
                    name = skill.name,
                    prompt = skill.prompt
                ));
                s.push_str(&format!(
                    "Goal achieved → skill_end (exits to {target}); rollback → \
                     abandon_skill. Security controls still apply to every tool \
                     call. If the user hand-edits a skill file mid-run, \
                     skill_reload re-reads the library (no restart).\n",
                    target = skill.target_state
                ));
            } else {
                // Defensive: Skill state with no active skill overlay. This
                // shouldn't happen (start_skill sets both atomically), but if
                // it does, tell the agent to end the skill so the workflow
                // recovers to a sane state.
                s.push_str(
                    "A skill is nominally active but no skill overlay is recorded. Call \
                     skill_end or abandon_skill to return to a normal workflow state.\n",
                );
            }
        }
        WorkflowState::Subagent => {
            // Parented sub-agent: the plan stack below is the READ-ONLY MIRROR
            // of the main plan (loaded from the shared plans dir by
            // build_inner) — orientation, not a work order. The sub-agent's
            // actual task arrives in its spawn prompt; it cannot mutate plans
            // and its tool surface comes from its spawn-time allow-list.
            if let Some(plan) = workflow.plan() {
                let parents = workflow.parent_titles();
                if parents.is_empty() {
                    s.push_str(&format!(
                        "PARENT PLAN (read-only context): {}\n",
                        plan.title
                    ));
                } else {
                    s.push_str(&format!(
                        "PARENT PLAN (read-only context, depth {}): {} ▸ {}\n",
                        workflow.plan_depth(),
                        parents.join(" ▸ "),
                        plan.title
                    ));
                }
                s.push_str(&format!("GOAL: {}\n", plan.goal));
                s.push_str(&format!(
                    "PROGRESS: {}/{} steps complete\n\n",
                    plan.completed_count(),
                    plan.steps.len()
                ));
            }
            push_state_text(&mut s, STATE_SUBAGENT);
        }
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemo2_prompt_append_is_scoped_to_the_blocked_spawn_policy() {
        let dir = tempfile::tempdir().expect("workflow dir");
        let policy = WorkflowPolicy::new(Arc::new(Mutex::new(Workflow::new(
            dir.path().to_path_buf(),
        ))))
        .with_blocked_tools(vec!["spawn_agent".to_string()]);
        let head = policy.stable_head(&KernelPromptBlocks::workflow(), &Constitution::default());
        assert!(head.contains("MNEMO2 TEAM RULES"));
        assert!(head.contains("Generic spawn_agent is unavailable"));
        assert!(head.contains("team_merge"));
        assert!(head.contains("never call git merge or git push"));
        assert!(head.contains("never read or edit team.json"));
        assert!(head.contains("SAME worker_id"));
        assert!(head.contains("no cumulative worker token or cost budget"));
        assert!(head.contains("team_compact"));
        assert!(!head.contains("increase the worker token budget"));
        // The lead delegates the bulk of the work to full executors and picks
        // each worker's model; it never degrades workers to read-only.
        assert!(head.contains("Workers do the majority of the work"));
        assert!(head.contains("Never brief a worker as read-only"));
        assert!(head.contains("Choose each worker's model and reasoning effort deliberately"));

        let ordinary = WorkflowPolicy::new(Arc::new(Mutex::new(Workflow::new(
            dir.path().to_path_buf(),
        ))));
        let ordinary_head =
            ordinary.stable_head(&KernelPromptBlocks::workflow(), &Constitution::default());
        assert!(!ordinary_head.contains("MNEMO2 TEAM RULES"));
    }

    // ── TeamLead mode (Phase 6: the plan board) ───────────────────────────

    use crate::app::plan_board::{PlanBoard, PlanDraft};
    use crate::app::team_ledger::fake::FakeLedger;
    use serde_json::json;

    fn new_workflow(dir: &std::path::Path) -> Arc<Mutex<Workflow>> {
        Arc::new(Mutex::new(Workflow::new(dir.join("plans"))))
    }

    fn board(dir: &std::path::Path) -> Arc<PlanBoard> {
        Arc::new(PlanBoard::new(dir.join("plans"), dir.join("reviews")))
    }

    fn team_policy(dir: &std::path::Path) -> (WorkflowPolicy, Arc<PlanBoard>) {
        let board = board(dir);
        let policy = WorkflowPolicy::new(new_workflow(dir)).with_team_lead(TeamLeadWiring {
            ledger: Arc::new(FakeLedger::passing()),
            board: board.clone(),
        });
        (policy, board)
    }

    fn denied(v: &GateVerdict) -> Option<&str> {
        match v {
            GateVerdict::Deny { message } => Some(message),
            _ => None,
        }
    }

    #[test]
    fn team_head_swaps_lifecycle_and_closing_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let (team, _) = team_policy(dir.path());
        let k = KernelPromptBlocks::workflow();
        let head = team.stable_head(&k, &Constitution::default());

        // The board text is in, the single-plan machinery is out.
        assert!(head.contains("a plan board"));
        assert!(head.contains("PLAN FIRST"));
        assert!(head.contains("plan_create"));
        assert!(head.contains("team_dispatch(plan_id, step, ...)"));
        assert!(head.contains("plan_update"));
        assert!(head.contains("plan_step_done"));
        assert!(head.contains("PLANS CLOSE AUTOMATICALLY"));
        assert!(head.contains("you are the reviewer of record through team_review"));
        assert!(head.contains("there is no finish call"));
        assert!(head.contains("Tiny direct edits by you"));
        assert!(head.contains("MNEMO2 TEAM RULES"));
        assert!(!head.contains("PLANNING:"));
        assert!(!head.contains("finish with no review_report"));
        assert!(!head.contains("spawn_agent role:"));
        assert!(!head.contains("The main agent can NEVER author a review"));
        assert!(!head.contains("- spawn_agent: parallel review/investigation"));
        // The universal rules and strategy survive the surgery.
        assert!(head.contains("Never commit to main"));
        assert!(head.contains("Final summaries"));
        assert!(head.contains("- memory_write:"));
        assert!(head.contains("- ask_user:"));
        assert!(!head.contains("reviewer subagent → fix findings"));

        // Classic: bytes unchanged (a pass-through constitution scope).
        let classic = WorkflowPolicy::new(new_workflow(dir.path()));
        let classic_head = classic.stable_head(&k, &Constitution::default());
        assert_eq!(
            classic_head,
            crate::agent::policy::assemble_stable_head(&k, &Constitution::default())
        );
        assert!(classic_head.contains("reviewer subagent → fix findings"));
        assert!(!classic_head.contains("PLAN FIRST"));
    }

    /// The TeamLead head splits `k.rules` and filters `k.strategy` by text;
    /// this fails loudly if the kernel blocks drop either marker.
    #[test]
    fn team_head_surgery_markers_exist_in_the_kernel_blocks() {
        let k = KernelPromptBlocks::workflow();
        let at = k
            .rules
            .find(RULES_CLOSING_MARKER)
            .expect("APP_RULES keeps its CLOSING SEQUENCE heading");
        let universal = &k.rules[..at];
        assert!(universal.contains("Never commit to main"));
        assert!(!universal.contains("spawn_agent"));
        assert!(
            k.strategy
                .lines()
                .any(|l| l.starts_with(STRATEGY_SPAWN_LINE)),
            "TOOL_STRATEGY keeps its '- spawn_agent:' line"
        );
    }

    #[tokio::test]
    async fn team_surface_is_free_and_hides_the_classic_plan_tools() {
        let dir = tempfile::tempdir().unwrap();
        let (team, _) = team_policy(dir.path());
        let surface = team.tool_surface().await;
        assert!(matches!(surface.advertise, Some(crate::tool::ToolFilter::Free)));
        assert!(matches!(surface.gate, Some(crate::tool::ToolFilter::Free)));

        // Team tools pass with no plan and no workflow state.
        for name in ["team_dispatch", "team_merge", "team_status", "plan_create"] {
            assert!(
                matches!(team.gate_call(name, &json!({})).await, GateVerdict::Allow),
                "{name}"
            );
        }
        // The classic plan tools and spawn_agent are refused.
        for name in TEAM_HIDDEN_TOOLS {
            let verdict = team.gate_call(name, &json!({})).await;
            assert!(denied(&verdict).is_some(), "{name}");
        }
        let verdict = team.gate_call("finish", &json!({})).await;
        assert!(denied(&verdict).unwrap().contains("plan_step_done"));
        let verdict = team.gate_call("spawn_agent", &json!({})).await;
        assert!(denied(&verdict).unwrap().contains("team_dispatch"));

        // Classic keeps its state-filtered surface.
        let classic = WorkflowPolicy::new(new_workflow(dir.path()));
        assert!(!matches!(
            classic.tool_surface().await.gate,
            Some(crate::tool::ToolFilter::Free)
        ));
    }

    #[test]
    fn team_factory_unregisters_the_hidden_tools() {
        use crate::agent::policy::AgentPolicyFactory;
        use crate::app::tool::workflow::plan::{CompleteStepTool, CreatePlanTool, FinishTool};

        let dir = tempfile::tempdir().unwrap();
        let wf = new_workflow(dir.path());
        let register = |reg: &mut ToolRegistry| {
            reg.register(Box::new(CreatePlanTool::new(wf.clone())));
            reg.register(Box::new(CompleteStepTool::new(wf.clone())));
            reg.register(Box::new(FinishTool::new(wf.clone(), dir.path().join("reviews"))));
        };
        let team = crate::app::policy_factory::WorkflowPolicyFactory::default()
            .with_team_lead(Arc::new(FakeLedger::passing()), board(dir.path()));
        let mut reg = ToolRegistry::new();
        register(&mut reg);
        team.finalize_tools(&mut reg);
        for name in ["create_plan", "complete_step", "finish"] {
            assert!(reg.get(name).is_none(), "{name} stays hidden");
        }
        // Classic keeps them.
        let classic = crate::app::policy_factory::WorkflowPolicyFactory::default();
        let mut reg = ToolRegistry::new();
        register(&mut reg);
        classic.finalize_tools(&mut reg);
        assert!(reg.get("create_plan").is_some());
    }

    #[tokio::test]
    async fn team_tail_is_the_compact_board_summary() {
        let dir = tempfile::tempdir().unwrap();
        let (team, board) = team_policy(dir.path());
        let caps = Capabilities::default();
        assert_eq!(team.volatile_tail(&caps).await, "# PLAN BOARD\nNo open plans.\n");

        let plan = board
            .create(PlanDraft {
                title: "Importer".into(),
                goal: "Fix it".into(),
                context: "Rows vanish in src/a.rs on empty cells; verify with cargo test.".into(),
                steps: vec!["Fix src/a.rs".into(), "Test src/b.rs".into()],
                ..Default::default()
            })
            .unwrap();
        board.tick(&plan.id, 0, "merged").unwrap();
        let tail = team.volatile_tail(&caps).await;
        assert_eq!(
            tail,
            format!("# PLAN BOARD\n- {} \"Importer\" 1/2 done; todo 2\n", plan.id)
        );
        assert!(!tail.contains("WORKFLOW STATE"));

        // Classic tail is byte-identical to the shared builder.
        let wf = new_workflow(dir.path());
        let classic = WorkflowPolicy::new(wf.clone());
        assert_eq!(
            classic.volatile_tail(&caps).await,
            workflow_section(&*wf.lock().await)
        );
    }

    #[tokio::test]
    async fn team_turn_end_always_stops() {
        let dir = tempfile::tempdir().unwrap();
        let (team, _) = team_policy(dir.path());
        for unattended in [false, true] {
            let summary = crate::agent::policy::TurnSummary {
                success: true,
                stop_reason: None,
                unattended,
            };
            assert!(matches!(team.on_turn_end(&summary).await, Continuation::Stop));
        }
        assert_eq!(team.route().await.slots, vec!["lead".to_string()]);
    }
}
