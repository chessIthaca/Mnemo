# Plan: Harden the plan-first gate

## Goal
Close the three gaps that let a stale plan act as a standing license to edit:
(1) reset to Planning when a genuinely new task arrives, (2) require `create_plan`
for the first mutation of a new task, (3) surface the active plan so staleness is
visible. Make the plan-first rule solid, not advisory.

## Context
Root cause (confirmed in code): `AgentLoopFactory::build_inner` (`src/agent/factory.rs:201`)
calls `Workflow::load_latest()`, which loads the most-recent `.coding/plans/*.md` and
puts the agent straight into `Executing` if that plan has unchecked steps. A stale,
unrelated plan therefore exposes all write tools to any new request. The gate
(`ToolFilter::allows`, `src/tool/mod.rs:126`) only blocks writes in Planning/Complete,
so it can't tell whether the current task is the one that was planned.

## Steps
- [ ] 1. Workflow: add plan-freshness + staleness. Track a `created_at`/mtime and a
  `task_fingerprint` (hash of the first user message) on `PlanFile`; expose
  `Workflow::is_stale()` and `Workflow::reset_to_planning()`. Persist via plan file
  front-matter or sidecar. Add unit tests.
- [ ] 2. New-task detection at turn start. In `AgentLoop::run_turn`, when a new user
  message arrives and the workflow is Executing with a stale/fingerprint-mismatched
  plan, reset to Planning before building the tool schema. Wire the first user
  message into the fingerprint.
- [ ] 3. Per-task first-mutation gate. Add a session-scoped `planned_for_task` flag on
  `AgentLoop` (set when `create_plan` succeeds); hide write tools until it's set for
  the current task, independent of the persisted workflow state. Tests: write blocked
  pre-plan, allowed post-plan.
- [ ] 4. Surface the active plan. Extend the Executing prompt block + emit a
  `PlanInfo` IPC event (title, goal, progress, created_at, staleness) so the frontend
  PlanProgress view shows it and flags a stale plan. Update `get_workflow_state`.
- [ ] 5. Frontend: show plan title/progress/staleness badge in the status/plan area;
  a "new task → reset plan" affordance when stale. (React/TS.)
- [ ] 6. Tests + docs: `cargo test`, update `agent.md` plan-first wording to document
  the new-task reset behavior.
