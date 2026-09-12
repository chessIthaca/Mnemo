+++
title = "backlog item resolution paths — halt stamping is terminal for approvals"
created = "2026-09-01"
status = "superseded"
+++

SUPERSEDED 2026-12-06 by
`2026-12-06-backlog-status-plan-lifecycle.md` (backlog 45dcf577, plan
b921fccf): halt stamping is NO LONGER terminal for approvals — the plan-tied
contract removed every harness `Failed`/`CantResolve` stamp except root-plan
abandonment. The trace below records the PRE-45dcf577 behavior (historical).

Backlog run-all/single-dispatch resolution plumbing (traced 2026-12, plan
0a58db61): (1) halt_run_all (src-tauri/src/ipc/run_all.rs) stamped the
in-flight item Failed AND called end_run — an approval-halted item never
resolved later (run_all was cleared; run-all items never enter
single_in_flight), so halt's Failed stamp WAS the terminal status for
approvals. (2) A steer soft-stops the turn (StopReason::Steer → Finished,
src/agent/loop_impl.rs) and the steer re-runs as its own turn, so any
intervention latch set at steer time is consumed by that turn's resolution.
(3) An interrupt during run-all does NOT halt the run (only send_suggestion
halts). (4) Transition table (src/backlog.rs): Pending→InFlight,
Pending→{Done,Failed,CantResolve}, InFlight→{Done,Failed,CantResolve,Pending},
terminal→Pending. The 0a58db61 fix: InFlight stamped at Executing entry
(forwarder), not dispatch; steer/interrupt set a user_intervention latch;
on_main_turn_resolved consumes it and requeues the item non-terminally
instead of failing it.
