## Verdict: FINDINGS (0 high, 1 low)

Round-2 (final) verification of the "429 → automatic cross-provider model fallback" plan (id 6d221559) on `wt/agenticcoding` @ 914ca32. All 3 round-1 findings are **resolved as specified** and the production code is correct. One new low-severity test-strength gap: the LOW 2 test's mock resolver doesn't ping-pong, so it can't catch a removal of the `tried_fallback` guard — the exact regression it was meant to guard against.

## Finding resolution

### MEDIUM 1 — 429 fallback silently no-ops for the default provider — RESOLVED ✓

`try_429_fallback` (src/agent/loop_impl.rs:1219-1222) now reads exactly the suggested fix:
```rust
let model = self.resolved_model().or_else(|| {
    let m = self.provider().model().to_string();
    (!m.is_empty()).then_some(m)
})?;
```
mirroring `effective_provider_name()` (loop_impl.rs:726-739), which already falls back to `provider().provider_name()` on the next line.

**Test non-vacuous (traced).** `rate_limit_429_falls_back_on_default_provider_path` (src/runtime/agent.rs) uses `NoOverrideResolver` whose `resolve()` returns `None`. In `resolve_turn_provider` (loop_impl.rs:894-903) step 4: `resolver.resolve()` → `None` → `set_resolved_model(None)` + `set_resolved_provider(None)` + returns `None`, so `run_turn` runs on the default snapshot (the rate-limited `primary`). On 429, `try_429_fallback` reads `resolved_model()` = `None` → `provider().model()` = "test-model"; `effective_provider_name()` → `provider().provider_name()` = "primary"; excludes "primary", finds "fallback", pins via `set_explicit_provider`, retries → success. Asserts primary_calls==1, fallback_calls>=1, saw_switching, saw_finished, !saw_terminal_error. **Without the fix** the old `resolved_model()?` returns `None` → no fallback → terminal error → `saw_finished=false`/`saw_terminal_error=true` → test fails. Genuine regression test. ✓

### LOW 1 — Stale doc comment on `is_non_retryable()` — RESOLVED ✓

src/error.rs:93-96 now reads: *"Transient errors (500/503/504/connect failures) remain retryable; a 429 is handled separately by [`is_rate_limited`](Self::is_rate_limited) — it skips same-provider retry and triggers the cross-provider fallback instead."* 429 is excluded from the retryable list and points to `is_rate_limited()`. Matches the suggested fix. ✓

### LOW 2 — No test for the "fallback also 429s" path — RESOLVED (test exists, exercises the path) ✓

`rate_limit_429_fallback_also_429s_fails_without_cascade` (src/runtime/agent.rs) has both primary and fallback rate-limited. Traced: 1st 429 → `tried_fallback=true`, switch to fallback; 2nd 429 → `!tried_fallback` false → `attempt=MAX` → final failure. Asserts primary_calls==1, fallback_calls==1, terminal error contains "429" + "no alternate provider". The "fallback also 429s → final failure" branch is genuinely exercised. ✓ (See new LOW finding below re: guard-removal coverage.)

## New finding

### LOW 3 — The LOW 2 test can't catch a removal of the `tried_fallback` guard (mock resolver doesn't ping-pong)

**Where:** `FallbackResolver::find_alternate_endpoint` (src/runtime/agent.rs, in the 429-fallback test module).

**Problem.** The mock returns `Some(fallback)` **only** when `exclude == "primary"` and `None` otherwise. After the first switch the current endpoint is "fallback", so a *second* `try_429_fallback` call (which would happen if the `tried_fallback` guard were absent) excludes "fallback" → the mock returns `None` → `try_429_fallback` returns `None` → `attempt=MAX` → final failure. The outcome is identical with or without the guard: primary_calls==1, fallback_calls==1, same terminal error. **Removing the `tried_fallback` guard leaves the test green** — so it does not verify the guard's anti-cascade effect, which was LOW 2's stated concern ("if someone removed the `tried_fallback` guard, the suite would not catch a cascade").

The real `ConfigModelResolver::find_alternate_endpoint` (src/model_resolver.rs:338-343) excludes **only the current endpoint** and returns the first *other* endpoint serving the model. With two endpoints both serving the model it **ping-pongs** (primary→backup→primary→…) indefinitely without the guard — an infinite loop / turn hang in production. The mock doesn't replicate this, so the test is blind to the regression it was meant to catch.

**Fix (trivial).** Make the mock faithful — return the *other* endpoint for either exclude value (mirroring the real resolver):
```rust
fn find_alternate_endpoint(&self, model_id: &str, exclude: &str) -> Option<ModelRef> {
    if self.has_alternate && model_id == "test-model" {
        let other = if exclude == "primary" { "fallback" } else { "primary" };
        return Some(ModelRef { endpoint: other.into(), model: "test-model".into() });
    }
    None
}
```
With that mock, removing the guard → primary↔fallback ping-pong → the 5s deadline expires with no terminal error → `terminal_error_text.expect(...)` panics → test fails. (The `rate_limit_429_falls_back_to_alternate_endpoint` test, whose fallback *succeeds*, is unaffected since it stops after one switch either way.)

## Other checks (no issues)

- **MEDIUM 1 accessor correctness.** On the default-provider path `provider()` is the default-slot provider that just served (and 429'd) — `provider().model()` + `effective_provider_name()` are the correct `(model_id, endpoint)` pair to exclude. On the per-context-override path `resolved_model()`/`effective_provider_name()` carry the override's values and `provider()` is not read. Both paths correct. ✓
- **`is_rate_limited()` ordering.** `run_turn_attempt` checks `is_rate_limited()` before `is_non_retryable()`; a 429 is rate-limited but not non-retryable (verified by `rate_limited_is_distinct_from_non_retryable`), so a 429 always takes the fallback path. ✓
- **`complete_with_retry`** (dispatch.rs:629) returns immediately on `is_rate_limited()` — no 3× same-provider backoff. ✓
- **Terminal error** (agent.rs:387-417): 429-with-no-alternate surfaces the qualifier "rate limited (429), no alternate provider found" + actionable hint; non-429 errors surface raw text. ✓
- **Multi-platform neutrality.** Pure Rust string-matching + `std::time::Instant` (test deadlines only). No Windows-only APIs, no `cfg(windows)`. ✓
- **Docs sync.** README inflight-bar bullet updated with the 429 behavior; doc comments present on `is_rate_limited()`, `find_alternate_endpoint()`, `try_429_fallback()`, `FallbackInfo`; LOW 1 doc now correct. ✓
- **Build.** Could not run `cargo test` (read-only reviewer). Plan reports `cargo test --lib` = 1686 passed / 0 failed / 0 warnings under `#![deny(warnings)]`; the diff introduces no `#[allow(...)]` and no obviously dead code. ✓ (trust)

## Summary

All 3 round-1 findings are resolved and the production code is correct. The single new low finding is a test-strength gap (unfaithful mock) with a trivial 2-line fix — it does not affect shipped behavior, only the suite's ability to catch a future guard-removal regression.
