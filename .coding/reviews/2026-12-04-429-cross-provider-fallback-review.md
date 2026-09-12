## Verdict: FINDINGS (1 medium, 2 low)

Review of the "429 → automatic cross-provider model fallback" plan (id 6d221559) on `wt/agenticcoding`. The core mechanism is sound — the priority chain, the "stop after a single 429" two-layer guard, the `tried_fallback` single-switch cap, the classification, and the actionable terminal error are all correct and well-tested for the per-context-override + reviewer cases. One medium functional gap (the fallback silently no-ops for the default-provider case, the most common configuration) and two low findings (stale doc, missing edge-case test).


### MEDIUM 1 — 429 fallback silently no-ops for the default provider (no per-context override)

**Where:** `src/agent/loop_impl.rs` — `try_429_fallback` (line ~1228): `let model = self.resolved_model()?;`

**Problem.** `resolved_model()` returns `None` whenever a turn ran on the **default provider** — i.e. when `resolve_turn_provider` fell through the whole chain (no skill override, no picker pin, no forced model, no `[models.<state>]` override) and returned `None` (loop_impl.rs:887–903 calls `set_resolved_model(None)` on that path). `try_429_fallback` propagates that `None` via `?` and returns `None` immediately, so **no fallback is attempted** and the turn goes straight to the "no alternate provider found" terminal error — even when a backup endpoint serving the same model *is* configured.

This is the most common agent configuration: the main agent running on its default model with no `[models.*]` state overrides. The plan goal states the fallback "Works for all agents including the reviewer subagent"; the reviewer (forced-model) path works, but the plain main-agent-on-default path does not.

**Confirmation it's an oversight, not a design choice.** `effective_provider_name()` (loop_impl.rs:726–739) — read on the very next line of `try_429_fallback` — *does* fall back to `self.provider().provider_name()` when `resolved_provider` is `None`. So `current` resolves to the default endpoint's name, but `model` is `None` and the function returns before `current` is ever used. The asymmetry between the two accessors is the tell: one handles the default case, the other doesn't.

**Impact.** A 429 on the default provider surfaces "rate limited (429), no alternate provider found … Switch models via the status-bar picker" even though an alternate endpoint exists. The turn fails gracefully (no crash), but the automatic recovery the feature promises doesn't fire for the baseline configuration. Neither integration test covers this: both use a `FallbackResolver` whose `resolve()` always returns `Some(ModelRef{…})`, so `resolved_model()` is always `Some`.

**Fix.** Fall back to the default provider's model id when no override was resolved, mirroring `effective_provider_name()`:
```rust
let model = self
    .resolved_model()
    .or_else(|| {
        let m = self.provider().model().to_string();
        (!m.is_empty()).then_some(m)
    })?;
```
`self.provider()` is the default-slot provider (the one that just served — and 429'd on — the turn when no override was active), so `provider().model()` + `effective_provider_name()` are the correct `(model_id, endpoint)` pair to exclude. Add an integration test where the resolver's `resolve()` returns `None` (default-provider path) and a 429 still falls back to the alternate.

---

### LOW 1 — Stale doc comment on `is_non_retryable()` (429 no longer "retryable")

**Where:** `src/error.rs:92-94` — the `is_non_retryable()` doc comment.

The comment still says: *"Transient errors (429/500/503/504/connect failures) remain retryable."* After this change a 429 does **not** remain retryable via same-provider backoff — `complete_with_retry` returns immediately on `is_rate_limited()` (dispatch.rs:629) and `run_turn_attempt` routes it to the fallback path. The new `is_rate_limited()` doc correctly explains the distinction, but the `is_non_retryable()` doc now contradicts the actual behavior for the 429 case. Suggest updating to exclude 429 (e.g. "Transient errors (500/503/504/connect failures) remain retryable; a 429 is handled separately by `is_rate_limited()` — it skips same-provider retry and triggers the cross-provider fallback.").

---

### LOW 2 — No test for the "fallback also 429s" path (`tried_fallback` second 429)

**Where:** `src/runtime/agent.rs` — the `tried_fallback` guard (line ~313–335).

The `tried_fallback` flag is the key safety mechanism preventing a cascade through every endpoint. The two integration tests cover (a) fallback succeeds and (b) no alternate exists → terminal error. Neither covers the third branch: a fallback **was found and pinned**, the retry ran on it, and it **also returned 429** → `!tried_fallback` is now `false` → `attempt = MAX` → final failure (no third endpoint, no backoff). If someone removed the `tried_fallback` guard, the suite would not catch a cascade. Suggest a test with both `primary` and `fallback` providers rate-limited, asserting exactly 2 provider calls (1 primary + 1 fallback) and a terminal error carrying the 429 qualifier.



## Verified (focus points that check out)

1. **Priority chain (focus #1).** Confirmed in `resolve_turn_provider` (loop_impl.rs:813–909): step 1 skill override → step 2 `explicit_provider` pin (set by `set_explicit_provider`) → step 3 `forced_model` → step 4 resolver chain. The fallback pins via `set_explicit_provider` (step 2), which is checked **before** `forced_model` (step 3), so the fallback correctly overrides the reviewer's spawn-time forced model. ✓

2. **"Stop after a single 429" (focus #2).** Two-layer guard verified: (a) `complete_with_retry` (dispatch.rs:629) returns `Err` immediately on `is_rate_limited()` — no 3× same-provider backoff; (b) `run_turn_attempt` (agent.rs:312–335) sets `tried_fallback = true` before the first switch, and on a second 429 `!tried_fallback` is false → `attempt = MAX` → final failure. No cascade through endpoints, no backoff retry of a rate-limited provider. ✓

3. **`is_rate_limited()` classification (focus #3).** Matches "http 429", "too many requests", "rate limit", "limit exhausted" case-insensitively on `Error::Provider` only (error.rs:145–154). No overlap with `is_non_retryable()` patterns (verified by `rate_limited_is_distinct_from_non_retryable` test). Non-429 errors (500/502/context-overflow/auth/connect) correctly excluded (`non_429_errors_are_not_rate_limited`). The `if is_rate_limited() … else if is_non_retryable()` order in `run_turn_attempt` is correct — a 429 always takes the fallback path. ✓

4. **`find_alternate_endpoint` (focus #4).** Excludes the failed endpoint (`ep.name != exclude_endpoint && ep.has_model(model_id)`), returns the first other endpoint in config order, returns `None` for no-alternate / unknown-model / no-endpoints (model_resolver.rs:325–351). `has_model` (endpoints.rs:176) + `resolve_model_ref` (config/mod.rs:108) validate the endpoint exists. Five unit tests cover both directions + all None cases. ✓

5. **"No fallback" terminal error (focus #5).** User-facing `AgentEvent::Error{retrying:false}` augments the raw error with the qualifier + "Switch models via the status-bar picker, or configure another endpoint serving this model." (agent.rs:410–417). The model-facing in-context note uses the qualifier "rate limited (429), no alternate provider found" (agent.rs:387–404). Both paths verified. ✓

6. **Reviewer subagent (focus #6).** No `ask_user` added in the turn layer. The no-fallback case reuses the existing terminal-failure path (`return None`), so the parent's `reviewer_failure_pending` latch + ask-user protocol handles the "ask the user" step. ✓

7. **Security (focus #7).** Classification operates on locally-produced `Error::Provider(String)` messages — no user input, no new attack surface. Switching notes + terminal errors carry endpoint names and model ids (config values); no API keys or secrets logged. ✓

8. **Multi-platform neutrality (focus #8).** Pure Rust string-matching (`to_lowercase`/`contains`) + `std::time::Instant` (test deadlines only). No Windows-only APIs, no `cfg(windows)`. ✓

9. **Documentation sync (focus #9).** README inflight-bar bullet updated with the 429 behavior. Doc comments present on `is_rate_limited()`, `find_alternate_endpoint()`, `try_429_fallback()`, `FallbackInfo`. (LOW 1 above is the one stale doc.) ✓

10. **Build.** Could not independently run `cargo test` (read-only reviewer). The plan reports `cargo test --lib` = 1684 passed / 0 failed / 0 warnings under `#![deny(warnings)]`; the diff introduces no `#[allow(...)]` and no obviously dead code. ✓ (trust)

## Scope note

The deferred-swap interaction (`set_explicit_provider` defers when the fallback's `max_context` < the field's current `max_context`) was traced through `run_turn` (turn.rs:72–207): the summarization uses `self.provider` (the default slot), and on summarization failure the swap completes anyway with messages preserved — graceful degradation, not a correctness bug. Not raised as a finding.
