# Review: Retry mid-stream provider errors (connection reset)

**Date:** 2026-04-04
**Reviewer:** self-review (spawn_agent skill unavailable in this workflow state)
**Scope:** all uncommitted changes (`git diff HEAD`) — `src/agent/turn.rs`, `src/agent/tests.rs`, plus plan bookkeeping in `.coding/plans/`.

## Files changed
- `src/agent/turn.rs` — the `LlmEvent::Error` handler (~line 350) no longer emits `AgentEvent::Error { retrying: false }` immediately; it stores the error in `stream_error`. The no-partial-output branch (~line 434) now returns `Err(Error::Provider(...))` instead of `Ok(TurnOutcome{...})`. A `# Errors` doc-comment section was added to `run_turn`.
- `src/agent/tests.rs` — new test `midstream_error_with_no_output_returns_err`.
- `.coding/plans/*.md`, `.coding/plans/stack.json` — plan bookkeeping (not code).

## Summary

The fix routes a mid-stream provider error (connection reset / `os error 10054` / unexpected EOF) that arrives *before* any partial output to the existing outer `run_turn_with_retry` layer (`src/runtime/agent.rs:54-108`) by returning `Err` instead of `Ok`. The outer layer already retries with 1s/2s/4s backoff and emits `retrying: true` messaging. This is the correct, minimal fix — it reuses the existing retry machinery rather than adding a third retry loop. The condition `had_error && text.is_empty() && !acc.saw_tool_calls()` is the right gate: only retry when there's NO partial output, so a stream that produced usable content is not silently re-requested (which would duplicate the partial response / re-execute side-effecting tool calls).

## Findings

### CORRECTNESS (medium) — partial-output mid-stream error is now silently swallowed

`src/agent/turn.rs:350-353` (the `LlmEvent::Error` handler):

Previously the handler emitted `AgentEvent::Error { retrying: false }` **unconditionally** — whether or not partial output had arrived. That old behavior was itself buggy: emitting `retrying: false` mid-turn sets `running = false` in the frontend (`useAgentStore.ts:1032-1033`) while the turn kept running and eventually emitted `Finished`, leaving the UI in an inconsistent stopped-but-still-running state.

The fix correctly removed that unconditional emit (fixing the no-output case by returning `Err`). But it removed the emit for the **partial-output** case too: when a mid-stream error arrives *after* text or tool-call deltas, the error is now silently swallowed — `had_error` is set and `stream_error` is stored, but the no-output branch (`had_error && text.is_empty() && !acc.saw_tool_calls()`) is false, so the turn falls through to process the partial output (text/tool-calls) and the stored error is never surfaced. The user gets no signal that the stream was truncated.

This is a behavior change from the old code (which at least showed *something*, albeit incorrectly as a fatal error). The right fix: emit a **non-fatal** note (`retrying: true`) for the partial-output case so the truncation is visible, without stopping the agent (the partial output is still used).

**Recommended fix:** after the no-output branch returns `Err`, add a branch for `had_error` with partial output that emits `AgentEvent::Error { error: stream_error, retrying: true }` (a transient note — the frontend keeps `running = true`), then falls through to the existing partial-output processing.

### CORRECTNESS (low, info) — `stream_error` fallback string

`src/agent/turn.rs:443-444`: `stream_error.unwrap_or_else(|| "stream ended with an error before any output".into())`. The `LlmEvent::Error` handler always sets `stream_error = Some(error)` when it sets `had_error = true`, so the `None` branch is unreachable in practice. The fallback is defensive (correct — guards against a future code path that sets `had_error` without storing the error) and harmless. No action needed.

### CONSTITUTION COMPLIANCE (pass)
- `run_turn`'s doc comment was extended with a `# Errors` section explaining when it returns `Err`. ✅
- The new test has a doc comment. ✅
- `cargo test` passes (439 lib tests, 0 failed). ✅

## Verdict

The core fix is correct and minimal. One finding to address: surface a non-fatal note for the partial-output case so a truncated stream is visible to the user without stopping the agent.
