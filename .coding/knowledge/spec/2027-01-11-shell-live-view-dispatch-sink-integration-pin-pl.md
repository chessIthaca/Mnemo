+++
title = "shell live-view dispatch sink + integration pin (plan 0d2c1221 step 5)"
created = "2027-01-11"
+++

Plan 0d2c1221 "Stream shell tool output live while the command runs" — step 5 (dispatch wiring + integration pin), verified 2027-01-16 on wt/mnemo.

WIRING (src/agent/dispatch.rs, already in place and re-verified): `AgentLoop::output_sink(&self, fanin_tx, agent_id, tool_call_id) -> OutputSink` (dispatch.rs:628-646) builds a per-call sink that `try_send`s `AgentEvent::ToolOutputDelta { tool_call_id, stream, text }` on the fan-in channel; it is built once at the execution site (dispatch.rs:481) and cloned into `dispatch_with_interrupt` (dispatch.rs:607-616), which passes it to `ToolRegistry::dispatch_streaming` (src/tool/mod.rs:1087) — so BOTH the approved path and the auto-approved path stream. `try_send` drops a delta on a full channel by design (live view is pixels; the result is the truth).

INTEGRATION PIN (new, step 5): `agent::tests::dispatch_streams_shell_output_deltas_before_the_result` in src/agent/tests.rs (added right after `make_dispatch_fixture`). It drives the REAL `execute_tool_call` funnel — not just the sink — so it is the end-to-end dispatch-level pin:
- local registry: `ToolRegistry::new()` + `crate::tool::agent::shell::ShellTool::new(sandbox)` only (the shared `make_registry` has NO shell; adding shell there would perturb tool-surface assertions in other tests).
- `shell` is an Executing-state tool: the fixture calls `wf.create_plan(...)` first, else the dispatch ToolFilter denies it (hidden in Planning).
- Autonomous safety mode => `needs_approval` returns false, so the call takes the NO-APPROVAL dispatch call site — not the safety-rules auto-APPROVED shortcut (that branch needs a matching rule). All three call sites pass the same `output_sink`, so the streaming path is identical; the approval-gated sites are code-inspected, not pinned. Corrected after review L5 (2027-01-16), and the pin now asserts `unexpected.is_none()` so an unexpected approval-wait fails loudly instead of hanging.
- ordering PROVEN, not raced: child prints `one`, blocks on `marker.txt`, then prints `two`; the test writes the marker only AFTER receiving the first delta, and the call cannot return before the child exits. Reuses the proven Windows command shape `[Console]::Out.WriteLine(...)` (PowerShell's formatter buffers `Write-Output` when stdout is redirected — using it would defeat the incremental check).
- `tokio::join!(call, watch)` with disjoint borrows (call: &fanin_tx + &mut cmd_rx/deny_all_latched/stop_signal; watch: &mut fanin_rx); the watch future records the first non-delta event into `unexpected` so a failure names its cause, and writes the marker on BOTH paths so the failure it is designed for — a non-streaming regression — can never hang the suite. It does NOT unblock a call that unexpectedly waits on an approval oneshot (the marker frees the CHILD, not the approval), which is why the pin now asserts `unexpected.is_none()` and fails loudly instead.

GOTCHA worth remembering: the FUNNEL NEVER EMITS `AgentEvent::ToolResult` — that is emitted by the turn loop (src/agent/turn.rs, `AgentEvent::ToolResult` emission) AFTER `execute_tool_call` returns, on the same FIFO channel. The pin therefore asserts deltas-while-running, then emulates the turn loop's result emission and asserts the result is LAST in channel order.

EVIDENCE: root suite green 2409 passed / 0 failed / 5 ignored (+16 integration, +0); `cd src-tauri; cargo check` exit 0. One full-suite run showed `agent::tests::constitution_reread_after_agent_md_edit` failing — that is the documented pre-existing agent_md mtime flake (see REVIEW: 2026-08-13-agent-md-mtime-flake-review), not this change; it passes in isolation and in the next full run.
