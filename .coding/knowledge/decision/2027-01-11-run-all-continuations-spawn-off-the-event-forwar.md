+++
title = "run-all continuations spawn off the event forwarder"
created = "2027-01-11"
+++

The event forwarder (src-tauri/src/ipc/events.rs::spawn — the single task recv()-ing the bounded fan-in channel, capacity 256) must NEVER await run-all dispatch/landing work inline: git checkpoints, embedder recall, worktree provisioning, and landings run seconds-to-tens-of-seconds under DISPATCH_LOCK/LANDING_LOCK, and an inline await fills the channel and freezes every agent's event delivery app-wide (2027-01-09 perf review L2 / BUG 3).

Fix (commit af567a7, plan a0d3defd): all ten resolution→dispatch continuation invocations (on_spawned_turn_resolved / on_main_turn_resolved — six in the forwarder loop, four in the two deferred-flush helpers) go through the private helper spawn_resolution_continuation (tokio::spawn). Contract: the latch evidence (workflow_changed / plan_abandoned / abandoned_plan_id) is read BEFORE the spawn (the next Started clears it); latch consumption, the wf_complete gate, and remark_pending_finished stay inline; DISPATCH_LOCK/LANDING_LOCK remain the serialization points (compact_then_dispatch_next already ran spawned on this contract). Done-counter bumps on BOTH the exit drains and the continuation side are gated on BacklogStore::transition's bool return — exactly one bump per item across the continuation-vs-exit-drain race.

Pinned by: resolution_continuation_tests (events.rs — paused-clock behavioral + source-contract), exit_drain_done_bumps_the_run_counter + continuation_done_bumps_are_gated_on_the_transition (run_all.rs). Reviews: .coding/reviews/2026-09-11-run-all-forwarder-uninline-review{,-round2,-round3}.md.
