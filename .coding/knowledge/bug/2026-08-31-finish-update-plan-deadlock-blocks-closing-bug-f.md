+++
title = "finish↔update_plan deadlock blocks closing bug_fixing plans"
created = "2026-08-31"
+++

BUG: finish↔update_plan deadlock — bug_fixing plans can't be closed out.

**Symptom:** finish errors "bug_fixing plan has no regression test recorded — the verify step must call update_plan with regression_test before finish." But calling update_plan in Reviewing errors "tool 'update_plan' is not allowed in the current workflow state (Reviewing)." The only escape is abandon_plan → re-create → re-record during Executing (absurd for a one-field update).

**Root cause:** 4 stacked guards create the deadlock:
1. ToolFilter::Reviewing (src/tool/mod.rs:425-436) — the Workflow arm explicitly excludes update_plan. Comment: "the plan is finished, so no step or new-plan work is allowed mid-review."
2. dispatch.rs:102 — filter.allows() returns false → "tool 'update_plan' is not allowed in the current workflow state (Reviewing)"
3. mod.rs:510 — `if self.state != WorkflowState::Executing` → WorkflowWrongState
4. plan.rs:1256-1268 — finish gate blocks without plan.regression_test

The natural order fights the gate: the regression test name is often only known AFTER running tests (end of verify step), but complete_step(4) flips to Reviewing before you can record it.

**Fix (planned, plan 853421fe):** Two paths (user chose "Both"):
- Part A: relax update_plan for regression_test-only in Reviewing — add update_plan to the Reviewing ToolFilter allow-list (tool/mod.rs:425); change the mod.rs:510 state guard to allow Reviewing when only regression_test is provided (title/goal/context/steps stay frozen).
- Part B: finish accepts an optional regression_test param (FinishArgs:1036) that records the test on the plan frame via wf.update_plan before the gate check.

**Regression tests:** update_plan_regression_test_allowed_in_reviewing (mod.rs) + finish_accepts_regression_test_param (plan.rs). Both fail before the fix, pass after.

Key files: src/tool/mod.rs (ToolFilter::Reviewing:402, test:1600), src/workflow/mod.rs (update_plan:499, test:1991), src/tool/workflow/plan.rs (FinishArgs:1036, finish execute:1182, UpdatePlanTool schema:560, finish tests:3119), src/agent/dispatch.rs (filter check:102), src/error.rs:38.
