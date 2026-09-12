## Verdict: PASS (all 4 low findings resolved)

Original verdict: FINDINGS (0 high, 4 low) — all 4 low findings resolved in commit 1c979c4:

Review of uncommitted changes for plan **a1f3ce04** ("Cut provider waiting: fail-fast timeouts, 502 context guard, and cache-stabilizing token quantization"). The three in-scope mechanisms — 30s→10s connect timeouts, proxy context-overflow 502 classification as non-retryable, and 2048-token quantization of `max_completion_tokens`/`max_tokens` — are **functionally correct**: the timeout constants and `fetch_models_*` builders are all 10s; `is_non_retryable` lowercases before matching (so the all-lowercase patterns correctly catch mixed-case `ContextWindowExceededError`); and the quantization arithmetic is panic-free (non-zero const divisor, `saturating_sub` floors underflow, `.max(1024)` floors the result, floor-division never exceeds the context window). No high-severity correctness, security, or regression issues. The 4 low findings are doc-sync / robustness nits.

**Scope note (co-mingled WIP):** the working tree also carries substantial unrelated work in `src/provider/stream.rs` (the identity/fragment `merge_identity` split) plus the co-located H2/H3 raw-echo guards in `openai.rs`/`anthropic.rs` (`raw_content_is_usable`, `is_repeated_block_type`, the H3 `type`-discriminator guard in `raw_is_usable`, and their tests). Per the task these belong to a different plan and were **not** reviewed here; they should land under their own plan/commit, not be folded into this one's commit.


---

## Findings

### LOW-1 — Stale "30s" connect-timeout references in provider doc comments

The `CONNECT_TIMEOUT` const doc comments were correctly updated to mention 10s (openai.rs:111-116, anthropic.rs:99-104), but several companion doc comments in the same changed files still say "30s" and are now factually wrong:

- **src/provider/openai.rs:80-81** — `http_client` field doc: *"A 30s *connect* timeout bounds the handshake"*
- **src/provider/openai.rs:226** — `fetch_models_anthropic` doc: *"30s connect / 15s total timeout"* (code at :237 is `from_secs(10)`)
- **src/provider/openai.rs:281** — `fetch_models_with_vision` doc: *"30s connect / 15s total timeout"* (code at :295 is `from_secs(10)`)
- **src/provider/openai.rs:743-744** — stream error-attribution comment: *"the full connect timeout (30s) elapsed before the failure"*
- **src/provider/anthropic.rs:68-69** — `http_client` field doc: *"A 30s *connect* timeout bounds the handshake"*

**Fix:** change each "30s" → "10s" in those five spots. (The two `CONNECT_TIMEOUT` const docs that intentionally reference the *old* 30s as motivation — openai.rs:113 and anthropic.rs:101, "burning 30s per attempt" — are correct as written and should stay.)

### LOW-2 — README.md still cites a "30s connect timeout"

**README.md:61** — *"…so a 30s connect timeout or 90s dead-connection stall is attributed instead of lost"*. The connect timeout is now 10s; this user-facing example is stale.

**Fix:** "a 10s connect timeout or 90s dead-connection stall".

### LOW-3 — Anthropic test comment arithmetic is off by 4

**src/provider/anthropic.rs** — `max_tokens_quantized_when_prompt_large` (the comment above the assertion, ~line 1907): states *"6000 chars -> prompt_est 1500"*. `estimate_prompt_tokens` (mod.rs:714) is `messages.len() * 4 + chars / 4` = `4 + 1500` = **1504**, not 1500. The final assertion (6144) is still correct either way (7472 and 7476 both floor-quantize to 6144), so the test passes — but the intermediate is wrong and would mislead a future reader debugging the math. The OpenAI counterpart test gets this right ("prompt_est = 4 + 6000/4 = … 1504").

**Fix:** "6000 chars -> prompt_est 1504 -> context_budget = 10000 - 1504 - 1024 = 7472 -> quantized 6144".

### LOW-4 — `input_tokens` + `maximum` conjunction is broad and not independently tested

**src/error.rs:130** — `|| (s.contains("input_tokens") && s.contains("maximum"))`. This is the broadest of the new patterns: any error mentioning both words is classified non-retryable (fail-fast, no retry / no cross-provider fallback). A non-context-overflow message that happens to carry both — e.g. a hypothetical TPM/quota body *"input_tokens exceeds the maximum per-minute rate"* — would be misclassified as permanent instead of retryable/rate-limited. The realistic target messages do carry both words, and the style matches existing broad conjunctions (`model` + `not found`), so the practical risk is low.

Additionally, the pattern has **no isolated test**: `context_overflow_is_non_retryable` case 4 (*"context_window_exceeded: input_tokens 150000 exceeds maximum allowable tokens"*) also matches the earlier `context_window_exceeded` pattern, so removing the conjunction would not fail any test — it is unpinned.

**Fix (optional, defense-in-depth):** tighten to require an overflow verb, e.g. `(s.contains("input_tokens") && (s.contains("exceeds") || s.contains("exceeded") || s.contains("too long") || s.contains("greater than")))`, and add a test case that matches *only* the conjunction (e.g. *"input_tokens 150000 is greater than the maximum 128000"*) so the pattern is pinned. At minimum, add the isolated test so the pattern can't silently regress.


---

## Per-area assessment

### 1. Correctness — PASS
- **Timeouts:** all four in-scope connect-timeout sites are 10s — `OpenAiClient::CONNECT_TIMEOUT` (openai.rs:117), `AnthropicClient::CONNECT_TIMEOUT` (anthropic.rs:105), `fetch_models_anthropic` (openai.rs:237), `fetch_models_with_vision` (openai.rs:295). A repo-wide `connect_timeout` scan found no remaining 30s site in the provider layer (the only other `connect_timeout` calls are `mcp/http.rs`/`mcp/oauth.rs` `HANDSHAKE_TIMEOUT`, unrelated and out of scope).
- **Error string matches:** `is_non_retryable` does `msg.to_lowercase()` (error.rs:111) *before* every `.contains()`, so the all-lowercase patterns (`contextwindowexceedederror`, `reduce the length of the input prompt`, …) correctly match mixed-case provider strings like `ContextWindowExceededError`. The new `context_overflow_is_non_retryable` test exercises real LiteLLM 502/400 bodies. Verified each test case's match path by hand.
- **Quantization math:** `TOKEN_QUANTUM = 2048` (non-zero const ⇒ no div-by-zero); `context_budget = max_context.saturating_sub(prompt_est).saturating_sub(1024)` (saturating ⇒ no underflow); `quantized = (budget / 2048) * 2048` floors *down* ⇒ `quantized ≤ budget`, so the cap never loosens / never exceeds the context window; final `.max(1024)` floors the result so a sub-quantum budget still yields a usable 1024, never 0. OpenAI jitter test (6000→6144, 6300→6144) and the updated `max_completion_tokens_capped_to_context_window` (7472→6144) both check out against `estimate_prompt_tokens = messages.len()*4 + chars/4` (mod.rs:714). The Responses-API path (`build_responses_request_json`) sends no `max_completion_tokens`, so it is correctly untouched (no jitter source there).

### 2. Multi-platform neutrality — PASS
Pure Rust: `Duration::from_secs`, `usize` saturating/integer arithmetic, and `str::to_lowercase`/`contains`. No platform APIs, paths, or shell syntax. Builds and behaves identically on Windows and macOS.

### 3. Docs sync — FINDINGS (LOW-1, LOW-2, LOW-3)
The `CONNECT_TIMEOUT` const doc comments were updated to 10s, but five companion doc comments in the changed files and one README line still say "30s" (LOW-1/LOW-2). One test comment has an off-by-4 arithmetic slip (LOW-3). No `endpoints.toml` or `PLAN.md` timeout references are affected.

### 4. Security / regression — PASS (one robustness nit, LOW-4)
- No DoS / panic / integer hazard in the new arithmetic (see Correctness).
- The behavior change — classifying proxy context-overflow 502s as non-retryable — is the intended fix: context overflow is permanent (the prompt doesn't shrink between attempts), so the 3×3 retry stack was always doomed; fail-fast is correct, not a regression. Transient 502/503/504/connect errors remain retryable (verified by `transient_errors_remain_retryable`, which still asserts `"HTTP 502 from gateway"` is retryable — a bare 502 without a context-overflow body is NOT caught by the new patterns).
- LOW-4: the `input_tokens` + `maximum` conjunction is the one pattern that could over-classify a non-overflow message as non-retryable; risk is low but the pattern is also unpinned by tests.

### Verification basis
Read-only review: `git diff HEAD` (full stat + hunks), the three changed source files in full context, `estimate_prompt_tokens` (mod.rs:695-715), and a repo-wide `connect_timeout` / `30s` literal scan. Tests were not re-run by the reviewer (read-only); the implementer's reported `cargo test` (1847 passed, 0 warnings under `#![deny(warnings)]`) was cross-checked against the test logic by reading each new/updated test.
