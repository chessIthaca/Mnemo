+++
title = "closing tiny findings one review_fix at a time no longer escalates a healthy worker"
supersedes = "2026-11-09-close-several-tiny-review-findings-in-one-round"
created = "2026-11-15"
evidence = "tests lead_review_fix_rounds_do_not_escalate, finding_open_after_a_worker_turn_still_escalates, legacy_review_rounds_without_a_turn_count_never_escalate"
+++

review_fix still needs a fresh delta team_review before the next fix, so closing N tiny findings one at a time means N review rounds. Since the escalation fix, a finding only counts as "stayed open" when the worker had a turn since the previous review (WorkerRecord.review_turns, src-tauri/src/mnemo2/team/review.rs), so lead-only review_fix rounds never trigger the repeated-finding nudge or an Auto tier switch. Real failed worker rounds still escalate. Regression test: lead_review_fix_rounds_do_not_escalate (team/tests.rs).
