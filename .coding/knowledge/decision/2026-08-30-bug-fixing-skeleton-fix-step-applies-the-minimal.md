+++
title = "bug_fixing skeleton fix step applies the minimal fix directly (no sub-plan)"
supersedes = "2026-08-28-bug-fixing-skeleton-fix-step-pushes-a-minimal-ch"
created = "2026-08-30"
+++

DECISION 2026-09-18 (user; DECISION memory 544111c5 is the authoritative rationale): The bug_fixing plan's locked skeleton step 3 applies the minimal fix DIRECTLY in the bug plan — it does NOT push a sub-plan. This REVERSES the 2026-02-13 decision (plan 1ef2bfa3, commit f46076e) that made step 3 push a minimal-change sub-plan. Root rationale: a bug_fixing sub-plan can only itself be bug_fixing (nested bug-fixing is invalid — a sub-plan of a bug plan would just re-copy the skeleton's own 4 steps into the child), so the sub-plan step was a plan-loop generator producing lots of wasteful action (user aborted plan b840f00c over exactly this mid-session, 2026-09-18). Code: BUG_FIXING_SKELETON step 3 in src/tool/workflow/plan.rs (~:44-45) + test pin in create_plan_bug_fixing_forces_skeleton_and_persists_symptom + PLAN.md bug_fixing bullet. The arrow shorthand reproduce→root-cause→fix→verify (README, prompt.rs, run_all.rs) stays accurate: the fix phase exists and is now applied directly.
