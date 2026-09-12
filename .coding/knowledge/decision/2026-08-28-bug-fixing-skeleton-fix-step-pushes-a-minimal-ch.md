+++
title = "bug_fixing skeleton fix step pushes a minimal-change sub-plan"
created = "2026-08-28"
status = "superseded"
+++

DECISION 2026-02-13: The bug_fixing plan's locked skeleton step 3 no longer applies the minimal fix directly — it pushes a SUB-PLAN (create_plan, default implementation kind) whose steps implement the fix with the LEAST amount of change possible, written self-contained (explicit file paths, exact edits, root cause + failing test name as context) so a lesser model can execute it. Rationale (user): some bug fixes are too big to implement in one step; a sub-plan structures the work. Commit f46076e on wt/agenticcoder (plan 1ef2bfa3, review PASS 0 findings, 1566 tests green). Code: BUG_FIXING_SKELETON in src/tool/workflow/plan.rs (~:35-50) + test pin in create_plan_bug_fixing_forces_skeleton_and_persists_symptom + PLAN.md bug_fixing bullet. Sub-plans never trigger their own review — the popped-to parent bug plan still enters Reviewing (workflow/mod.rs:665), so the review gate is unchanged. The arrow shorthand reproduce→root-cause→fix→verify (README, prompt.rs, run_all.rs) stays accurate: the fix phase exists, only its mechanism changed.
