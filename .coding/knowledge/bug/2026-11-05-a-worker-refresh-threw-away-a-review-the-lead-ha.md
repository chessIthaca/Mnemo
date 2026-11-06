+++
title = "a worker refresh threw away a review the lead had just recorded"
created = "2026-11-05"
+++

Symptom: merging a worker right after reviewing it intermittently failed with "the current worker tip has not been reviewed" (seen on the macOS CI runner in lead_merges_from_inside_its_own_turn). Root cause: pump_worker's transcript refresh in src-tauri/src/mnemo2/team.rs reads the worker's git tip outside the ledger lock; when the lead commits and reviews while that read is in flight, the refresh compared the older tip with the new review and cleared the review and its checks. Fix: clear_review_for_a_newer_tip keeps any review whose reviewed_at_ms is at or after the moment the tip read started; validate_merge still re-reads the live tip and every check tip, so a kept review can never merge an unreviewed commit. Regression test: a_refresh_that_read_the_tip_before_the_review_keeps_that_review (team.rs).
