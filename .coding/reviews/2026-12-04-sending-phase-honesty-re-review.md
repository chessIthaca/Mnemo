## Verdict: FINDINGS (0 high, 1 low)

Round-2 re-review of the "sending-phase honesty" plan (branch `wt/agenticcoder`, all uncommitted changes vs HEAD). All 5 round-1 findings are **behaviorally resolved** and verified against the actual source (not just the diff). One new LOW remains: a stale comment in the L5 test that now contradicts its own behavior. No correctness, security, or multi-platform defects.

## Round-1 finding resolution (all 5 RESOLVED)

### L1 — openai.rs 5 unstamped `log.fail()` sites — RESOLVED ✅

All 5 sites now stamp `set_ttft_ms`/`set_generation_ms` immediately before `log.fail()`, mirroring the anthropic.rs pattern exactly. Verified in `src/provider/openai.rs`:

| Site | Line | Stamp |
|------|------|-------|
| mid-stream `LlmEvent::Error { error }` | 1044-1052 | generation (if `first_chunk`) / ttft (else) |
| "stream aborted — consumer dropped" | 1075-1086 | generation / ttft |
| "repetition detected" | 1102-1109 | generation / ttft |
| `SseOutcome::ParseError` | 1120-1127 | generation / ttft |
| body-decode error (`Err(e)` arm) | 1153-1160 | generation / ttft |

`first_chunk` (`Option<Instant>`, declared line 855) and `request_start` (`Instant`, declared line 835) are in scope at every site (they live in the spawned stream task). The pattern `if let Some(fc) = first_chunk { set_generation_ms } else { set_ttft_ms }` is byte-identical to anthropic.rs (lines 1073-1081, 1093-1104, 1108-1117, 1140-1147). The two pre-stream sites (connect failure line 731, HTTP-error line 770, retry-connect line 798) stamp `set_ttft_ms` via `record_created.elapsed()` — unchanged from round 1, still correct. openai.rs and anthropic.rs are now at full parity (9/9 and 7/7 paths stamped).

### L2 — native-Anthropic context-overflow wording — RESOLVED ✅

`is_non_retryable()` (error.rs) now includes `s.contains("prompt is too long")` in the context-overflow arm, with a comment explaining it covers the native Anthropic Messages-API body (`"prompt is too long: N tokens > M maximum"`). A native-Anthropic context-overflow is now classified non-retryable instead of burning the 3×3 retry stack.

### L3 — turn-level non-retryable skip had no regression test — RESOLVED ✅

`run_turn_attempt_skips_retry_for_non_retryable_error` (agent.rs:1024) drives the **full agent run**: it spawns `AgentTask::run`, sends a `Prompt`, and asserts (a) `calls == 1` (one provider call, not 3) and (b) zero `AgentEvent::Error { retrying: true }` notes. Verified the test genuinely reaches `run_turn_attempt`: `run` → `drive_turns` (agent.rs:196) → `run_turn_attempt` (agent.rs:125). Without the `attempt = MAX_PROVIDER_TURN_ATTEMPTS` line, `run_turn` would be called 3× (calls == 3) and 2 retrying notes would fire — so the test fails-without/pass-with the fix. Valid regression test.

### L4 — non-retryable errors got "retries exhausted" note — RESOLVED ✅

The in-context note now branches via a `qualifier` local (agent.rs:351-355): `"non-retryable provider error"` when `e.is_non_retryable()`, else `"provider error, retries exhausted"`. The format string `"[harness note] The previous LLM request failed terminally ({qualifier}): {truncated}"` carries the right wording on each path. The actionable error text (e.g. the context-overflow message) is still appended either way.

### L5 — `phase_waiting_emitted_on_provider_error_path` was ~3s — RESOLVED (behavior) ✅

The test now constructs `ContextOverflowProvider` (non-retryable) instead of the retryable `FailingProvider`. With a non-retryable error, `complete_with_retry` returns `Err` after a single `provider.complete()` call (the `is_non_retryable()` short-circuit at dispatch.rs:621 fires before any backoff sleep), so the test is now instant — no 1s/2s sleeps. The test still proves what it set out to (`Phase::Waiting` fires before the request on the error path, `Sending` precedes `Waiting`).

## New finding

### N1 — LOW (stale comment / doc inconsistency) — L5 test comment contradicts its own behavior

`phase_waiting_emitted_on_provider_error_path` (src/agent/tests.rs) was correctly swapped to the non-retryable `ContextOverflowProvider` (the L5 fix), but the comment that *justified* the old ~3s slowness was left in place and now reads falsely:

```rust
// Note: complete_with_retry retries 3× with 1s/2s backoff (~3s); this
// is acceptable for a single regression test.
```

With `ContextOverflowProvider`, `complete_with_retry` hits the `is_non_retryable()` short-circuit (dispatch.rs:621) on the first call and returns `Err` immediately — **0 retries, 0 backoff sleeps, ~0ms**, not ~3s. The comment describes the pre-fix behavior (the retryable `FailingProvider`). A future reader is misled into thinking the test is slow when it is in fact instant, and the "acceptable for a single regression test" rationale no longer applies to anything.

**Fix:** delete the two comment lines (or replace with a one-liner noting the provider is non-retryable so the test is instant). Trivial — comment-only, no code change.

## Overall correctness / security / docs / multi-platform

**Correctness — PASS.**
- `Phase::Waiting` move (turn.rs:893-907): emitted before `complete_with_retry`; the old post-`stream_result?` site is fully removed (no double-emission). On the error path `stream_result?` no longer swallows `Waiting`. The `[Sending, Waiting, Streaming, …]` relative order is preserved (Waiting still precedes Streaming).
- `is_non_retryable()` (error.rs): string-based classification on `Error::Provider(String)` — the existing pattern (provider errors fold HTTP status + body into the message text). Matches every observed gateway format; transient errors (429/500/502/503/504/connect/stream-stall) correctly stay retryable. The `to_lowercase()` normalization is applied once. No false-positive risk beyond the inherent fragility of substring matching (already accepted in round 1).
- `attempt = MAX_PROVIDER_TURN_ATTEMPTS` (= 3, agent.rs:25): for a non-retryable error, `attempt = 3` → `if 3 < 3` is false → final-failure `else` branch (in-context note + `Error{retrying:false}` + `return None`). Skips both the turn-level backoff sleep and the retrying-note emission. Defense-in-depth with the provider-level skip (dispatch.rs:621) — both correct.
- `set_ttft_ms`/`set_generation_ms` (trace.rs): route through `with_record` (file-writer mirror sees the mutation; no-op for unknown/evicted ids). `log.fail()` sets only `http_status`+`error` and never resets the timing fields, so every stamp survives into the terminal record. No double-stamping hazard (a second `set_*` just overwrites with a newer value).

**Security — PASS.** No new attack surface. The classification operates on locally-produced error strings, not user input; no injection or path-handling changes. No secrets logged (error text was already being recorded pre-change).

**Documentation sync — PASS (modulo N1).** README bullets accurately describe the new Sending=local-prep / Waiting=network-bound semantics, the non-retryable fail-fast behavior, and error-record timing attribution. `PhaseKind` doc comments (channels.rs) updated to match. `is_non_retryable()`, `set_ttft_ms`, `set_generation_ms` all carry doc comments. The only doc gap is the stale test comment (N1).

**Multi-platform neutrality — PASS.** All new code is pure Rust: `String::to_lowercase()` + `contains()`, `std::time::Instant::elapsed()`, `as_millis() as u32`. No Windows-only APIs, paths, or shell syntax anywhere in the library changes. Builds and behaves identically on macOS and Windows.

**Tests — PASS.** `cargo test --lib` reported by the main agent: 1669 passed, 0 failed, 0 warnings (the `#![deny(warnings)]` crate root means zero warnings is proven). New regression coverage: `is_non_retryable` unit tests (6 cases), `set_ttft_ms`/`set_generation_ms` trace tests (2), `complete_with_retry_does_not_retry_context_overflow` (provider-level), `run_turn_attempt_skips_retry_for_non_retryable_error` (turn-level), and the updated `phase_waiting_emitted_on_provider_error_path`.

## Summary

The plan's three goals are correctly and completely implemented. All 5 round-1 findings are resolved. The single remaining LOW (N1) is a two-line stale comment in a test — a trivial comment-only fix; no code or behavior change is needed.
