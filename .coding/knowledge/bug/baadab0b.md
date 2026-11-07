+++
title = "A merged worker's stuck checkout is retried until it is gone"
created = "2026-11-07"
+++

Symptom: A merged worker whose checkout could not be removed (Windows os error 32, usually a shell whose working folder was inside the worktree) keeps the folder on disk and the note "merged, but worktree cleanup needs attention: ..." on its record for ever, and its branch is never deleted, because nothing ever retries the removal. · regression test: a_later_merge_retries_an_earlier_merged_workers_stuck_checkout

Full record for plan baadab0b (see .coding/plans/baadab0b.md for the plan file).

regression test: a_later_merge_retries_an_earlier_merged_workers_stuck_checkout · path .coding/plans/baadab0b.md · branch wt/mnemo @ 1e87edaa (unmerged — exists only on this branch)
