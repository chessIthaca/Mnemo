+++
title = "idle-compaction tests waited for the savings row with a fixed yield count and flaked on loaded CI"
created = "2026-11-14"
evidence = "CI run 43632496108 red at agent.rs:8238; after the fix the idle_compaction tests passed 8/8 in 3 local runs (and 5 runs plus a full workspace run by the worker)."
+++

Symptom: `runtime::agent::tests::idle_compaction_uses_the_measured_lifetime_and_records_the_row` failed on CI ("the idle compaction must persist an idle_compaction savings row") on a docs-only PR while passing locally.
Root cause: the row is written by `AgentLoop::record_savings_event` (src/agent/turn.rs) via `tokio::spawn` → `MemoryStore::record_savings_event` → `spawn_blocking` (a real OS thread). The tests run with `start_paused = true` and polled at most 100 times with `yield_now()`: a count, not a time, so a slow blocking pool on a loaded runner lost the race.
Fix: `wait_for_idle_compaction_row` in the src/runtime/agent.rs tests module polls until the row appears or a real 10 s `std::time::Instant` deadline (a tokio sleep would not wait under paused time); used by that test and `committed_cut_scenario`.
Lesson: under paused tokio time, wait for anything written on a `spawn_blocking` thread with a real-time deadline, never a fixed number of yields.
