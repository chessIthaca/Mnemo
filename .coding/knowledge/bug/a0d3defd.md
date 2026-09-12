+++
title = "Un-inline run-all dispatch from the event forwarder (perf review L2)"
created = "2027-01-11"
+++

Symptom: During a parallel run-all, the single event-forwarder task stops recv()-ing from the bounded fan-in channel (capacity 256, src/runtime/mod.rs:38) while it awaits turn-resolution→dispatch work inline — git checkpoint commits, embedder recall round-trips, worktree checkout/branch-fork provisioning (tens of seconds when multiple lanes provision), and serialized landings. The channel fills; every agent's fanin_tx.send(...).await blocks; mid-stream lanes stall (TCP backpressure, stall_ms accumulates); the UI shows nothing app-wide. No data is lost — everything freezes. (2027-01-09 perf review, finding L2 / executive-summary BUG 3, HIGH, run-all scoped.) · regression test: continuation_in_flight_does_not_block_event_flow

Full record for plan a0d3defd (see .coding/plans/a0d3defd.md for the plan file).

regression test: continuation_in_flight_does_not_block_event_flow · path .coding/plans/a0d3defd.md · branch wt/agenticcoding @ af567a7 (unmerged — exists only on this branch)
