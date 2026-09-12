## Verdict: FINDINGS (0 high, 5 low)

The three fixes are fundamentally correct: the `Phase::Waiting` move preserves the `[Sending, Waiting, Streaming, …]` invariant, the `is_non_retryable()` classification matches every observed gateway error format in `.coding/logs/provider-errors.jsonl`, the `attempt = MAX_PROVIDER_TURN_ATTEMPTS` trick routes correctly to the final-failure `else` branch, and `set_ttft_ms`/`set_generation_ms` survive `log.fail()` (which only sets `http_status`+`error`). Both regression tests fail-without/pass-with the fix. The findings below are all LOW — completeness, consistency, coverage, and cosmetic gaps, no correctness or security defects.

## What was verified (all PASS)

1. **Phase::Waiting move (turn.rs:893-907).** Waiting now emits *before* `complete_with_retry` instead of after `stream_result?`. The existing `phase_events_cycle_across_requests_and_tools` test asserts `[Sending, Waiting, Streaming, RunningTools, Sending, Waiting, Streaming]` — the relative order Sending→Waiting→Streaming is preserved (Waiting is still before Streaming), so the test stays valid. No double-emission (the old post-stream site was removed). On the error path, `stream_result?` no longer swallows Waiting. On an Interrupt during the request, phases become `[Sending, Waiting, Finished]` — harmless (frontend maps phase to a label only; `agentEventReducer.ts:233` / `InflightBar.tsx:43` carry no "Waiting ⇒ stream opened" assumption).
2. **`is_non_retryable()` (error.rs:99-119).** Matches all observed production formats: 401 `"LiteLLM Virtual Key expected"` + `"auth_error"` (lines 1-18 of the log), 502 context-overflow containing `"maximum context length"` (lines 25-36), 404 `"litellm.NotFoundError"` (test case). Transient errors (`"failed to start stream"`, `"stream aborted — consumer dropped"`, 429/502-gateway) correctly stay retryable. `provider_error` (openai.rs:538) emits `"…unauthorized (check the API key)"` for 401/403 → caught; includes the truncated body otherwise → context-overflow body caught.
3. **`attempt = MAX_PROVIDER_TURN_ATTEMPTS` trick (agent.rs:308-313).** `MAX_PROVIDER_TURN_ATTEMPTS = 3` (agent.rs:25). For a non-retryable error: `attempt = 3` → `if 3 < 3` is false → `else` (final failure: in-context note + `Error{retrying:false}` + `return None`). Skips both the turn-level backoff sleep and the retrying-note emission. Correct. The non-retryable check fires twice (provider-level in `complete_with_retry`, turn-level here) — defense in depth, both correct.
4. **Trace timing placement.** Every `set_ttft_ms`/`set_generation_ms` call precedes its `log.fail()`. `fail()` (trace.rs:572-577) sets only `http_status`+`error` and never resets `ttft_ms`/`generation_ms`, so the stamp survives into the terminal record (and thus into the `provider-errors.jsonl` line, which is appended when `with_record` flips `is_complete`). `record_created` (openai.rs:715 / anthropic.rs:841) is in scope at the connect/HTTP sites; `request_start`+`first_chunk` (declared inside the spawned task) are in scope at all mid-stream sites in both providers.
5. **Regression tests.** `phase_waiting_emitted_on_provider_error_path` — `FailingProvider` returns retryable `"always fails"`, so without the fix Waiting sat after `stream_result?` (skipped on Err) → assertion fails; with the fix Waiting emits before the request → passes. `complete_with_retry_does_not_retry_context_overflow` — without `is_non_retryable` the provider is called 3×; with it, 1× → assertion (`calls == 1`) gates the fix. Both are valid regression tests.
6. **Docs + multi-platform.** README bullets and `PhaseKind` doc comments (channels.rs) accurately describe the new Sending=local-prep / Waiting=network-bound semantics and the non-retryable + error-timing behavior. New code is pure Rust string-matching + `Instant::elapsed()` — no Windows-only APIs, paths, or shell syntax.


## Findings

### L1 — LOW (trace completeness / provider inconsistency) — openai.rs has 5 unstamped `log.fail()` sites; anthropic.rs stamps all equivalents

The plan goal #3 ("stamp ttft_ms/generation_ms on error records so elapsed time is attributed instead of lost") was applied **completely** in `anthropic.rs` (all 7 error paths stamped) but **only partially** in `openai.rs` (4 of 9 paths stamped). The 5 unstamped openai.rs sites, all inside the spawned stream task where `request_start` + `first_chunk` **are** in scope (so these are missed, not scope-limited):

- **openai.rs:1046** — mid-stream `LlmEvent::Error { error }` → `log.fail(*id, status, error)` with no stamp. The anthropic.rs equivalent (anthropic.rs:1073-1081) stamps generation/ttft.
- **openai.rs:1071** — `"stream aborted — consumer dropped"` → `log.fail(...)` with no stamp. Anthropic equivalent (anthropic.rs:1093-1104) stamps. **This is the single most common error in production** — `provider-errors.jsonl` has dozens of `"stream aborted — consumer dropped"` entries, all currently `ttft_ms:null`. After this fix they will *still* be null on the openai path.
- **openai.rs:1093** — `"repetition detected — stream aborted"` → no stamp. (No anthropic equivalent — the R10 repetition guard is openai-only.)
- **openai.rs:1106** — `SseOutcome::ParseError` → no stamp. Anthropic equivalent (anthropic.rs:1108-1117) stamps.
- **openai.rs:1134** — body-decode error (`Err(e)` arm) → no stamp. Anthropic equivalent (anthropic.rs:1140-1147) stamps.

**Impact:** on these paths the trace keeps `ttft_ms:null`/`generation_ms:null` — exactly the "lost timing" the fix set out to eliminate. The 3 primary targets named in the plan (connect failure, HTTP error, stream stall) ARE stamped in openai.rs, so this is a completeness gap, not a correctness bug.

**Fix:** mirror the anthropic.rs pattern at each of the 5 sites:
```rust
if let Some((log, id)) = &trace_ctx {
    if let Some(fc) = first_chunk {
        log.set_generation_ms(*id, fc.elapsed().as_millis() as u32);
    } else {
        log.set_ttft_ms(*id, request_start.elapsed().as_millis() as u32);
    }
    log.fail(*id, status.as_u16(), &error);
}
```

### L2 — LOW (classification false negative) — native-Anthropic context-overflow wording isn't matched

`is_non_retryable()` matches `"maximum context length"`, `"context length is"`, `"max_completion_tokens"`, `"max_model_len"`. A **native** Anthropic Messages-API context-overflow returns a body like `{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}`, which the shared `provider_error` (openai.rs:544-547) folds into `Error::Provider("stream request: HTTP 400 from {url} — {truncated_body}")`. That string contains **none** of the matched substrings ("200000 maximum" ≠ "maximum context length"), so a native-Anthropic context-overflow is classified **retryable** and burns the 3×3 retry stack.

**Not a regression** (pre-fix, nothing was classified) and the production gateway (LiteLLM) wraps Anthropic errors in OpenAI format containing `"maximum context length"`, so it's caught there. But the README advertises native Anthropic endpoints, and the plan's stated goal is "context-overflow … errors" without a gateway qualifier.

**Fix:** add `s.contains("prompt is too long")` to the context-overflow arm. (Optionally also `"input length and max tokens"`.)

### L3 — LOW (test coverage) — the turn-level non-retryable skip (`attempt = MAX_PROVIDER_TURN_ATTEMPTS`) has no regression test

`complete_with_retry_does_not_retry_context_overflow` (tests.rs) proves the **provider-level** skip (calls == 1) by calling `complete_with_retry` directly. It does **not** exercise `run_turn_attempt` (agent.rs:308-313), so the **turn-level** skip is untested: if someone removed the `attempt = MAX_PROVIDER_TURN_ATTEMPTS` line, `complete_with_retry` would still return after 1 provider call (the provider-level test stays green), but `run_turn_attempt` would retry `run_turn` 3× (3 turn-level backoff sleeps + 3 in-context notes) — a silent regression the suite wouldn't catch.

**Fix:** add a test that drives the full agent run (which goes through `run_turn_attempt`) with a `ContextOverflowProvider` and asserts the turn-level retry note is emitted **0** times (or that wall-time / call-count reflects a single attempt, not 3×). The existing `phase_waiting_emitted_on_provider_error_path` test calls `run_turn` directly and so also bypasses this layer.

### L4 — LOW (cosmetic / model-facing wording) — non-retryable errors get the "retries exhausted" in-context note

Routing non-retryable errors through the final-failure `else` branch (agent.rs:330-366) pushes the in-context note `"[harness note] The previous LLM request failed terminally (provider error, retries exhausted): {truncated}"`. For a non-retryable error **0 retries occurred**, so "retries exhausted" is inaccurate. The model still receives the actionable error text (e.g. the context-overflow message), so behavior is correct; this is a wording imprecision introduced by reusing the existing template for the new non-retryable path.

**Fix (optional):** branch the note text — e.g. `"(non-retryable provider error)"` when `e.is_non_retryable()`, else `"(provider error, retries exhausted)"`.

### L5 — LOW (test speed) — `phase_waiting_emitted_on_provider_error_path` is ~3s because it uses a retryable provider

The test uses `FailingProvider` (returns retryable `"always fails"`), so `complete_with_retry` sleeps 1s+2s across 3 attempts (~3s wall-time) — acknowledged in the test comment. The test only asserts `Waiting` is present and `Sending` precedes `Waiting`; it does **not** assert retry behavior. Using the non-retryable `ContextOverflowProvider` instead would make `complete_with_retry` return after 1 call (no sleeps, instant) while still proving Waiting fires before the request on the error path.

**Fix (optional):** swap `FailingProvider` for `ContextOverflowProvider` in that test (or add a dedicated instant variant) to drop the ~3s.
