+++
title = "Run-All interrupted items re-dispatch forever — intervention destroys the dispatch context"
created = "2027-01-07"
+++

Symptom: a Run-All item interrupted/steered mid-plan re-dispatches forever (45a4eb88: 3 dispatches, 2 completed plans, closed only by manual backlog_status). Root cause: the intervention path requeues InFlight→Pending AND ends the run (halt_run_all's steer arm + handle_user_intervention) — the plan stays active, the user's natural resume ("c") continues it with no current_item pointer, and the completing turn's resolution is blind (run gone, latch consumed, single_in_flight empty); the Executing-entry stamp can't re-link without a pointer, so every re-dispatch strands again. Fix (plan 28bc06a2): mirror the approval-halt — keep the item InFlight + the run (stop flag set, identity-guarded); still-open/terminal-error arms keep stopped runs alive (aligning with backlog_stop_all's "stop after the current item resolves"); single-dispatch items still requeue (their pointer is consumed — the queue is their only recovery). Regression tests: steer_halt_keeps_the_run_state_for_the_in_flight_item, intervention_keeps_a_run_all_item_in_flight_with_its_run, stopped_runs_survive_open_plan_and_error_turn_ends, intervention_handler_is_never_terminal_and_never_continues (src-tauri run_all tests).

Amended 2027-01-09 (plan cace17a6): single-dispatch items are now KEPT InFlight too (the handler restores the consumed single_in_flight pointer instead of requeueing) — the "single-dispatch items still requeue" sentence above is superseded.
