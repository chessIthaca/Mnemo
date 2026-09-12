+++
title = "update_plan callable in Reviewing (regression_test-only) + finish accepts regression_test param"
created = "2026-08-31"
+++

SPEC: update_plan is now callable in Reviewing (regression_test-only), and finish accepts an optional regression_test param — shipped on wt/agenticcoding (commit 8541504, 2026-12-04).

**Why:** The finish↔update_plan deadlock — finish required the regression_test name recorded via update_plan, but update_plan was blocked in Reviewing (the state finish runs in) by 4 stacked guards. Bug_fixing plans couldn't be closed out without the absurd abandon→re-create→re-record escape hatch. The regression test name is often only known after running tests (end of the verify step), but complete_step(4) flips to Reviewing before you can record it.

**New behavior:**
- `update_plan` in Reviewing accepts regression_test ONLY — title/goal/context/steps stay frozen (the workflow method's `has_structural_fields` guard rejects them). The ToolFilter::Reviewing allow-list now admits update_plan; the workflow method enforces the field restriction.
- `finish` accepts an optional `regression_test` param that records the test on the plan frame right before the gate check (via wf.update_plan). This is the one-call path: `finish(review_report=..., regression_test="...")`.
- Reviewers (ToolFilter::Reviewer) still do NOT get update_plan — the relaxation is Reviewing base-state only.

**Budget impact:** 3 context-budget ceilings raised — Executing 23_500→23_600, ExecutingResearch 20_300→20_400, Reviewing 18_500→20_700 (finish +37 chars, update_plan's full schema joins the Reviewing array).

Regression tests: update_plan_regression_test_allowed_in_reviewing (mod.rs) + finish_accepts_regression_test_param (plan.rs). Review: PASS.
