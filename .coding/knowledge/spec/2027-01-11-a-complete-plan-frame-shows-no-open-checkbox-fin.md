+++
title = "a Complete plan frame shows no open checkbox — finish ticks every step and preserves the appended cost report (plan 33a7341e)"
created = "2027-01-11"
+++

Invariant (plan 33a7341e, commit da11d8c, branch wt/mnemo): a plan that reaches Complete never renders an open checkbox. `Workflow::finish` (src/app/workflow/mod.rs:1066-1095) marks every still-unchecked step done in memory via `PlanFile::complete_all_steps` (top-level `## Steps` + every `## Detailed steps` checkbox) and then calls `PlanFile::persist_completed_marks` UNCONDITIONALLY (idempotent; heals a retried finish after a transient write error).

On-disk persistence is a mark flip, not a re-serialize: `persist_completed_marks` (src/app/workflow/plan_file.rs) walks the frame line by line, tracks the current `## ` section, and flips only `- [ ]` → `- [x]` inside `## Steps` / `## Detailed steps`, preserving every other byte (indent, text after `]`, LF/CRLF, trailing newline). This exists because FinishTool appends the per-plan `## Cost report` to the frame BEFORE `wf.finish()` runs — a serialize-over-file rewrite would drop it. A `- [ ]` literal quoted in Goal/Context is prose and stays untouched; the missing-frame fallback recreates the frame via `write_to_dir`. Step TEXT is never dropped (the frame is the crash-resumption document).

Review-fix steps appended during Reviewing via `update_plan append=true` stay unchecked through the review (crash-resume as the active step) and are ticked by the finish transition. Regression tests: `finish_ticks_appended_review_fix_steps` (src/app/tool/workflow/plan.rs) — red pre-fix with two open appended steps on a Complete plan; `complete_all_steps_ticks_every_open_mark`, `persist_completed_marks_preserves_appended_sections`, `persist_completed_marks_skips_checkbox_literals_outside_steps` (plan_file.rs). Reviews: .coding/reviews/2026-10-21-33a7341e-finish-ticks-review.md (round 1: 0 high, 3 low) and -round2.md (PASS).
