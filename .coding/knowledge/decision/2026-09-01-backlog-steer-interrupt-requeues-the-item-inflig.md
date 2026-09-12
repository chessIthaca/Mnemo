+++
title = "backlog steer/interrupt requeues the item — InFlight starts at Executing"
created = "2026-09-01"
status = "superseded"
+++

DECISION: backlog intervention semantics — steer/interrupt is never a failure (2026-12-05, commit 58bdf1a on wt/agenticcoder, plan 0a58db61, review r3 PASS). New behavior: (1) a user steer or interrupt on the main agent mid-item records a user_intervention latch; turn resolution requeues the item non-terminally (Pending stays Pending+annotated, InFlight→Pending with the checkpoint sha preserved in the note), never dispatches next / auto-feeds, and ends only the run whose current_item matches the intervened item (a deferred Run-All started mid-turn survives). (2) An item leaves Pending ONLY when the workflow enters Executing (forwarder stamps InFlight via should_stamp_in_flight/stamp_backlog_in_flight) — pre-planning interventions leave the item queued. (3) If the agent absorbed the steer and still closed the plan loop, the item commits and marks Done (finish_captured_item_done). (4) Approval halts keep stamping Failed (terminal and load-bearing: halt clears run_all, later resolution can never see the item) — halt_run_all takes stamp_failed. plumbing facts: see SPEC memory 'backlog item resolution paths' + .coding/plans/0a58db61.md.
