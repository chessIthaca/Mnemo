+++
title = "backlog item status ⇄ plan lifecycle — failed means exactly 'the plan was abandoned'"
created = "2026-12-06"
status = "superseded"
+++

SUPERSEDED 2027-01-07 by the run-all orphans amendment (plan 5f6515f5, .coding/knowledge/spec/2027-01-07-backlog-item-status-plan-lifecycle-failed-means.md): the whole status⇄plan-lifecycle contract stands — but the dead-owner paths changed: the main-agent-exit drain now REQUEUES the item to Pending (checkpoint sha preserved) instead of keeping it InFlight, run-all start adopts crash-orphaned InFlight items, and the exit clears single_in_flight. The original spec's "the item keeps its status" drain claim is historical (pre-5f6515f5).

Backlog 45dcf577 (plan b921fccf): an item's status is derived from the plan dispatched for it — never from how the session ended.

- `in_flight` ⇔ a plan dispatched for the item is ACTIVE. Stamped when the workflow enters `Executing` (the `create_plan` moment — `stamp_backlog_in_flight`, src-tauri/src/ipc/events.rs), which also records the ROOT plan id on the item (`BacklogItem.plan_id`, the item↔plan linkage; refreshed on every Executing entry so a replacement plan re-links). Stays `in_flight` through interruptions, steering pauses, and sub-plan pushes (a sub-plan always pops back to its parent).
- `done` ⇔ the plan reached `Complete` — the turn-resolution plan-loop gate (`plan_loop_allows_done`: Complete + a real workflow transition observed this turn) → commit_success + Done.
- `failed` ⇔ ONLY root-plan abandonment: `abandon_plan` observed as an `Executing`/`Reviewing` → `Planning` transition (`is_root_plan_abandonment`), latched per-turn on `TurnResolveLatch.plan_abandoned`, applied at turn resolution AFTER the gate (an abandon followed by a successor plan that finishes still resolves Done).
- Every other turn end — loop open, resting Complete (no plan ran), unverifiable state, terminal Error (crash/provider exhaustion) — is NOT a failure: no status transition (`plan_open_note` annotates what happened), NO rollback (the work stays in the tree for a resumed session), and no further items are dispatched (the run halts on the Run-All path — a STOPPED run is kept alive until its item resolves, backlog b83e891f; auto-feed is suppressed on the single-dispatch path via `resolved_terminally`).
- Halt-for-approval stamps NOTHING: the run state is deliberately KEPT (the agent's turn is still live, blocked on the approval) so the post-approval turn resolution resolves the item under the same rules; the stop flag prevents the next dispatch. A main-agent exit while a run is active drains it (`drain_run_all_on_main_exit` — the item keeps its status, the note records why the run ended).
- A git checkpoint failure at dispatch leaves the item `Pending` (annotate only — infrastructure, not abandonment).
- Escape hatches: the deliberate `InFlight → Pending` deferral (the single-dispatch intervention requeue — a RUN-ALL item is KEPT InFlight with its run stopped instead, backlog b83e891f: the plan stays active and the turn that completes it resolves the item through the kept run state) and the terminal → `Pending` requeue.
- The `backlog_status` agent tool documents the same contract; `CantResolve` is the agent's explicit dead end — the harness resolution paths never set it.

Supersedes `2026-09-01-backlog-item-resolution-paths-halt-stamping-is-t.md` (halt stamping is no longer terminal for approvals).

Amended 2027-01-09 (plan cace17a6): the single-dispatch intervention requeue escape hatch named above is REMOVED — a steer/interrupt now keeps ANY dispatched item InFlight (a single-dispatch item's consumed `single_in_flight` pointer is restored check-and-set; only a run-all item whose run was drained before the intervention resolution requeues). Canonical contract: the 2027-01-09 amendment in `2027-01-07-backlog-item-status-plan-lifecycle-failed-means.md`.
