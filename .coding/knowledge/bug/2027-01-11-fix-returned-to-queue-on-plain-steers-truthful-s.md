+++
title = "Fix \"returned to queue\" on plain steers — truthful steer bookkeeping + requeue-cause alignment — MERGED into main (branch wt/stream-plain, 2026-10-18)"
supersedes = "9c2eb7a4"
created = "2027-01-11"
+++

Symptom: Backlog items show "steered by the user, returned to queue" on PLAIN steers — factually false and contradicting the steer spec (DECISION 581e39fa: a steer is a course correction, never a requeue). The steer-resolution match's shared suffix composer (src-tauri/src/ipc/run_all.rs:7148, `format!("{}, returned to queue", iv.reason)`) is stamped by the Pending arm (:7210-7213) on pre-execution items that never left the queue, and by the drained-run arm (:7198-7209) whose legitimate requeue note leads with the steer reason instead of the agent-exit cause. Live example: item 51f3be28. · regression test: intervention_pending_steer_note_never_claims_a_requeue

Full record for plan 9c2eb7a4 (see .coding/plans/9c2eb7a4.md for the plan file).

regression test: intervention_pending_steer_note_never_claims_a_requeue · path .coding/plans/9c2eb7a4.md · landed at 6a6253b on branch wt/stream-plain (work tip 32b6f99, 2026-10-18) — a PR against main is opened from this branch, awaiting human review; the landing becomes part of main when the human merges the PR (the merge commit is then one `git log main --merges` away).
