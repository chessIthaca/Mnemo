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
use crate::project::Constitution;
use crate::provider::Capabilities;
use crate::tool::{ToolRegistry, ToolResult};
use crate::app::workflow::{Workflow, WorkflowState};

/// The workflow agent's policy — byte-identical behavior to the pre-2a
/// direct reads (acceptance :785-788).
pub struct WorkflowPolicy {
    pub(crate) workflow: Arc<Mutex<Workflow>>,
    /// The owning agent loop, attached by the factory once the loop is
    /// final (Arc-wrapped) — 2a.6. Facts the Workflow doesn't carry (today:
    /// the subagent flag) read through this back-ref; an empty Weak (before
    /// the attach, or after the loop is dropped) reads as not-a-subagent,
    /// matching the flag's default.
    loop_ref: std::sync::OnceLock<std::sync::Weak<crate::agent::AgentLoop>>,
}

impl WorkflowPolicy {
    /// Wrap the loop's workflow (factory build path, 2a.2). The factory
    /// also calls [`WorkflowPolicy::attach_loop`] once the loop is Arc'd
    /// (2a.6).
    pub fn new(workflow: Arc<Mutex<Workflow>>) -> Self {
        Self {
            workflow,
            loop_ref: std::sync::OnceLock::new(),
        }
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

#[async_trait]
impl AgentPolicy for WorkflowPolicy {
    fn stable_head(&self, k: &KernelPromptBlocks, c: &Constitution) -> String {
        crate::agent::policy::assemble_stable_head(k, c)
    }
    fn register_tools(&self, _reg: &mut ToolRegistry, _ctx: &PolicyBuildCtx) {
        // D5: registration stays kernel-called in 2a (factory fns run as today).
    }
    async fn volatile_tail(&self, _caps: &Capabilities) -> String {
        // 2a.4: the real delegate — lock the workflow once, assemble the
        // same bytes the kernel's workflow_section produced pre-2a.4.
        let wf = self.workflow.lock().await;
        workflow_section(&wf)
    }
    async fn tool_surface(&self) -> ToolSurface {
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
    async fn gate_call(&self, _name: &str, _args: &Value) -> GateVerdict {
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
        let route_key = {
            let wf = self.workflow.lock().await;
            let is_subagent = self
                .loop_ref()
                .and_then(|weak| weak.upgrade())
                .map(|loop_| loop_.is_subagent())
                .unwrap_or(false);
            crate::model_resolver::build_route_key(
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
        let unattended =
            Some(text.starts_with(crate::runtime::channels::UNATTENDED_PROMPT_PREFIX));
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
        let wf = self.workflow.lock().await;
        let state = wf.state();
        if matches!(
            state,
            WorkflowState::Executing | WorkflowState::Reviewing
        ) || (t.unattended && state == WorkflowState::Planning)
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
                    s.push_str(&format!("PARENT PLAN (read-only context): {}\n", plan.title));
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
