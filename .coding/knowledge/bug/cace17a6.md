+++
title = "Single-dispatch steer keeps the item in flight (plan⇔backlog sync)"
created = "2027-01-07"
+++

Symptom: Steering or interrupting a SINGLE-DISPATCHED backlog item requeues it to Pending while its plan stays active — the item falls out of sync with the plan lifecycle. Live 2027-01-09: item 0296d448 (the .git sandbox-escape fix) was steered mid-plan; handle_user_intervention requeued it to Pending ("steered by the user, returned to queue") AND consumed the single_in_flight pointer, so the continuation turn that completed plan eda38891 resolved nothing — the item sat Pending with finished work. User model (2027-01-09): as long as the plan is active, the backlog item is active — status linked to the plan lifecycle, pending being only the pre-dispatch exception. The run-all strand of this exact damage class was already fixed (backlog b83e891f, plan 28bc06a2: steer keeps a run-all item InFlight with its stopped run); the single-dispatch strand kept the requeue deliberately ("their pointer is consumed — the queue + auto-feed is their only recovery", decision 2027-01-07-run-all-intervention-pauses-the-item-kept-inflig) — that asymmetry is what this plan removes. · regression test: intervention_keeps_a_single_dispatch_item_in_flight

Full record for plan cace17a6 (see .coding/plans/cace17a6.md for the plan file).

regression test: intervention_keeps_a_single_dispatch_item_in_flight · path .coding/plans/cace17a6.md · branch wt/agenticcoding @ 02dd4cd (unmerged — exists only on this branch)
