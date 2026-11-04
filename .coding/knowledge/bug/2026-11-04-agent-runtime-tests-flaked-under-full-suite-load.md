+++
title = "Agent-runtime tests flaked under full-suite load because of a 500 ms idle break"
created = "2026-11-04"
+++

Symptom: rate_limit_429_high_fill_rate_does_not_regress_window_viability (src/runtime/agent.rs) failed intermittently in a full `cargo test --lib` run and passed when run alone. Root cause: the test drains read fan-in events with `timeout(500ms, recv)` and broke out on the first 500 ms quiet gap; under parallel load, tokenizing the ~60k-token prompt took longer than that, so the loop ended before Finished. Fix: a tests-module helper `next_event(rx, deadline)` waits until an overall 30 s deadline with `timeout_at`, and every drain in the file uses it while keeping its own exit conditions. Regression test: next_event_waits_out_a_gap_longer_than_the_old_idle_break. Verified with 5 stress runs of `cargo test --lib runtime::agent -- --test-threads=32`.
