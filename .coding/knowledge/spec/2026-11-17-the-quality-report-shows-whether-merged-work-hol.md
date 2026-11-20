+++
title = "the quality report shows whether merged work holds up, per tier and model"
created = "2026-11-17"
evidence = "quality.rs compute tests; QualityView.test.ts"
+++

src-tauri/src/mnemo2/quality.rs reads this project's rows from the machine-wide routing log plus the committed learnings bundle (filtered by laya_learnings::is_project_row, de-duplicated by line), along with plan kinds, git, and .coding/reviews/findings.jsonl.

Window: 30 days by default; the tab offers 7, 30 and 90.

Per final tier and per final model (the last of models_used) it reports:
- merged, and first_pass (no fix round and no high finding);
- average fix rounds and findings, and the sum of high findings;
- reverted (a `reverted` row at or after the outcome);
- bug-fix follow-ups (a later merge of a DIFFERENT bug_fixing plan within 14 days changing an overlapping file, via merge_commit on outcome rows);
- cost per merge.
Plus the recurring finding categories.

Where it appears: the lead tool team_quality, the IPC command quality_report, and the Quality tab in the side panel (QualityView.svelte).

Gaps: rows written before merge_commit existed are counted as missing for follow-ups; reverts done by hand in git are not logged.
