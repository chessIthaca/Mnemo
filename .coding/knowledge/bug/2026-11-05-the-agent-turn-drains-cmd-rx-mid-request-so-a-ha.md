+++
title = "the agent turn drains cmd_rx mid-request, so a hanging provider can't fill the command queue"
created = "2026-11-05"
+++

In this repo the headless agent's command channel holds **64** slots (`src/host/runtime.rs:405`, `mpsc::channel(64)`), and the agent turn polls `cmd_rx.recv()` *beside* the pending provider future — `select! { result = &mut complete_fut, cmd = cmd_rx.recv() }` (src/agent/turn.rs ~4805), repeated in the mid-stream loop (~:5196); compaction matches via `context.rs::summarize_with_interrupt`.

Consequence: **the agent drains queued commands while a provider request is still pending**. A test that tries to provoke `try_send` -> `Full` by sending many `Compact` commands to a turn held open by a hanging provider therefore races the consumer. `Session::send` -> `agent_command` awaits the manager mutex (plus the sessions/parked locks) BEFORE reaching `AgentManager::send`'s `try_send`, and tokio mutex acquisition consumes coop budget (initial 128), so the sender task yields and lets the consumer run. On a fast/multi-core runner the consumer wins and the queue never fills; on Windows the sender usually wins. This is not a queueing bug ��� it is a test-determinism problem.

Important corollary: **no provider behaviour can wedge that `select!`**, because the hanging `complete()` is itself a branch of it. Making the provider hang harder does NOT stop the drain; the only deterministic lever is a production test hook that pauses the turn loop, or an OS-level `#[cfg_attr(..., ignore)]` on the platform where the race falls the other way.

Related platform fact: the browser tool family is 10 offscreen tools on every platform plus 6 on-screen WebView2 tools registered only under `#[cfg(windows)]`, so `--features browser` adds 16 on Windows but 10 on macOS. Tool-count assertions must compute `10 + if cfg!(windows) { 6 } else { 0 }` (42/41 on Windows, 36/35 on macOS, 26/25 without the feature).
