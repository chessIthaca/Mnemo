+++
title = "OpenSpec close idempotency — marker written last gates the retry"
created = "2026-11-05"
+++

OpenSpec plan-close idempotency contract (src/app/openspec/archive.rs, src/app/plan_board.rs): the archive marker `.openspec/changes/archive/*/.openspec.yaml` (`mnemo_plan: <plan id>`) is written LAST inside `PreparedOpenSpecClose::write`, after every current-state spec and archive file — its presence on disk means the whole OpenSpec side of a close completed. `write_closed` calls `archive::already_archived(openspec_dir, id)` and skips `prepare_close`/`write` when a marker names the plan, so a close that failed only at the review-report write or board save retries without re-applying deltas (otherwise an ADDED requirement conflicts with itself and the plan can never close). Convention: any new file write in the close path must join `writes` BEFORE the marker, never after it. Unreadable/invalid markers count as absent; the normal close path re-runs and reports the real conflict. Regression test: `a_close_retries_after_the_archive_succeeded_but_the_report_failed` in src/app/plan_board.rs.
