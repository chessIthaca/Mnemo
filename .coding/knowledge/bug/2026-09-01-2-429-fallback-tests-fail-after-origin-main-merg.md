+++
title = "2 429_fallback tests hung after origin/main merge — test teardown deadlocked on a full fan-in channel"
created = "2026-09-01"
updated = "2026-09-02"
status = "resolved"
+++

Symptom: 2 of 4 `429_fallback` tests in src/runtime/agent.rs did not pass after
merging origin/main into the local working line:
- `default_path_429_fallback_does_not_pin`
- `state_override_survives_429_fallback`

Both are the tests that call `create_plan_with_kind`, which puts the workflow
into `Executing`. First they failed `assert_eq!(primary_exec_calls, 1)` with 9;
once that assertion was relaxed to `>= 1` they stopped failing and instead HUNG
indefinitely at zero CPU, wedging the whole suite and leaving orphaned
`mnemo-*.exe` test binaries behind across sessions.

Root cause — NOT what the first pass of this note recorded. The 429 fallback
logic was never at fault and does not pin the agent to the fallback provider.
Two separate things were misread:

1. The 9-vs-1 call count is CORRECT behavior, not a defect. While the workflow
   is `Executing`, a normal turn end makes `run_turn_with_retry` push a
   synthetic "continue from where you left off" and run another turn, bounded by
   `MAX_AUTO_CONTINUE = 12` (agent.rs:59). `AgentEvent::Finished` is emitted only
   after that whole chain completes, so all 9 exec-model calls have genuinely
   happened by the time the test asserts. `== 1` encoded the pre-dispatch.rs
   single-call-per-turn pipeline and was stale — the merge did not break it, it
   revealed it.

2. The hang was a bounded-channel backpressure deadlock in TEST TEARDOWN. Every
   event is `let _ = fanin_tx.send(..).await` on a channel of capacity 64.
   `wait_for_turn_outcome` breaks on the first `Finished`, so the test stops
   draining while the agent is still auto-continuing. The channel fills, the loop
   parks in `send().await`, and it never returns to `cmd_rx.recv()` to observe the
   dropped `cmd_tx` — so `handle.await` waited forever. Only the two plan-creating
   tests hit it; the other two never enter `Executing`, never auto-continue, and
   stay under the 64-event budget.

Fix: `shutdown_agent` helper (agent.rs, beside `wait_for_turn_outcome`) — drop
`cmd_tx`, spawn a task that drains `fanin_rx` while the loop winds down, and wrap
`handle.await` in a 10s timeout so any recurrence is a loud test failure rather
than a hung suite. Both tests keep `>= 1` for the exec-call count, with the
`fallback_*_calls` equality beside it as the sharp assertion that actually proves
the fallback did not pin.

Verified 2026-09-02: `cargo test --lib` 1771 passed / 0 failed / 16 ignored;
`cargo test --tests` 16 passed / 0 failed / 3 ignored; frontend `vitest run`
703 passed across 51 files.

Lesson: a test that stops reading a bounded channel while the producer is still
running is a deadlock, not a slow test. Zero CPU on a "hanging" test binary is
the tell — it is parked on an await, not spinning.
