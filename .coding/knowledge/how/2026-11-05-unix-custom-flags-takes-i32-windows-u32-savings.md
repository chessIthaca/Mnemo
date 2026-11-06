+++
title = "unix custom_flags takes i32 (windows u32); savings-ledger tests must poll the spawned write"
created = "2026-11-05"
+++

Two durable rules, both learned from a failed tag build (commit b81e0fdd).

1) `OpenOptions::custom_flags` takes DIFFERENT types per platform extension: `std::os::unix::fs::OpenOptionsExt::custom_flags(&mut self, flags: i32)` vs `std::os::windows::fs::OpenOptionsExt::custom_flags(&mut self, flags: u32)`. A shared `O_NOFOLLOW`-style flag constant must therefore be typed per cfg block — declaring the unix ones `u32` compiles on Windows (those blocks are cfg'd out) and fails on macOS/Linux with E0308 "expected i32, found u32". Values: O_NOFOLLOW linux 0x20000, macOS 0x100. Windows-only local checks can never prove a cfg(unix) block; only a unix target or CI can.

2) The savings ledger is written fire-and-forget: `AgentLoop::record_savings_event` (src/agent/turn.rs) emits the live event synchronously but `tokio::spawn`s the durable `record_savings_event` store write. So any test that asserts on `store.savings_events_rows(..)` right after a turn (or right after calling the persistence method directly) MUST poll until the row appears — a single read or a fixed `sleep` races a slow runner ("a savings row names the sentinel: []"). src/agent/tests.rs carries the shared helper `wait_for_savings_row(store, matches, timeout)` (25 ms polls, 5 s default) for exactly this; reuse it instead of hand-rolled loops or sleeps. The production fire-and-forget behaviour is intended — never make the test green by awaiting the write in production.
