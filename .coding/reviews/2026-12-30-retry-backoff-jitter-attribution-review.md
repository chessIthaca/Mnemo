## Verdict: FINDINGS (0 high, 3 low)

Review of ALL uncommitted changes on `wt/agenticcoding` (git diff HEAD + untracked) for plan 26c01541 "Retry backoff: equal jitter + turn-level trace attribution". The implementation is correct and complete: the equal-jitter helper is right (bands, shift cap, saturating_sub, no-panic clock fallback), both retry layers use it with correct attempt indexing, the parked value is the actual slept duration in both layers, the 429/non-retryable fail-fast paths still neither sleep nor park, and all four regression tests match the real retry flow. The only findings are three stale doc comments still describing the retired fixed "1s, 2s, 4s" backoff ladder.

## Findings

### L1 (Low, doc sync) — `src/agent/dispatch.rs:674-675`: `complete_with_retry` doc comment still says fixed "1s, 2s, 4s backoff"

> /// Call `provider.complete` with retry + exponential backoff.
> /// Retries up to 3 times on provider errors, with 1s, 2s, 4s backoff.

Stale on two counts after this change: the backoff is now jittered equal jitter (attempt 1 → 500–1000 ms, attempt 2 → 1000–2000 ms — see `retry_backoff_ms`), and "4s" was never reachable even before (the `if attempt < 2` guard allows at most 2 sleeps). The code below this comment and the `MAX_PROVIDER_TURN_ATTEMPTS` doc were both updated; this one was missed. Fix: e.g. "Retries up to 3 times on provider errors, with jittered exponential backoff (equal jitter, ~0.5–1s then ~1–2s — see [`crate::error::retry_backoff_ms`])."

### L2 (Low, doc sync) — `src/agent/dispatch.rs:12-14`: module doc comment, same stale ladder

> //! `complete_with_retry` wraps the provider's streaming `complete` with
> //! exponential backoff (1s, 2s, 4s) so a transient gateway error doesn't
> //! kill the turn.

Same retired fixed ladder in the `//!` module header. Fix: e.g. "with jittered exponential backoff (equal jitter — see `retry_backoff_ms`) so a transient gateway error doesn't kill the turn."

### L3 (Low, doc sync) — `PLAN.md:374-375`: design doc still says "exponential backoff (1s, 2s, 4s)"

> Provider errors retry with exponential backoff (1s, 2s, 4s).

PLAN.md is explicitly in the project constitution's documentation-sync list (technical decisions). The retry design changed: jittered equal jitter via the shared `retry_backoff_ms` helper, used by both retry layers, with turn-level sleeps now attributed to the trace. Fix: e.g. "Provider errors retry with jittered exponential backoff (equal jitter via `retry_backoff_ms`: ~0.5–1s, ~1–2s) so concurrent agents retrying the same recovering endpoint don't stay in lockstep."

Not flagged, deliberately: historical point-in-time records (old plan files, past review reports, root `reviews/`, the 2026-08-25 how-file) keep their original text; the authoritative spec (`.coding/knowledge/spec/2026-12-30-trace-phase-attribution-connect-backoff-stall-sp.md`) is updated in this diff. `README.md:61` "backoff (retry sleeps before this attempt)" remains accurate with jitter — the sleeps still precede the attempt; no README change needed.

## Verification detail

### 1. Jitter formula (`src/error.rs`, `retry_backoff_ms`) — correct
Equal jitter verified: `nominal = 1000u64 << attempt.saturating_sub(1).min(4)`; `delay = nominal/2 + nanos % (nominal/2 + 1)` → inclusive band `[nominal/2, nominal]`. Attempt 1 → [500,1000], 2 → [1000,2000], 3 → [2000,4000]; shift capped at 4 → nominal saturates at 16 s, no overflow possible (`1000 << 4 = 16000`). `saturating_sub` makes attempt=0 degrade gracefully to the attempt-1 band (no underflow panic). `duration_since(UNIX_EPOCH).map(...).unwrap_or(0)` — no panic on clock skew; falls back to the fixed half. Doc comment on the pub fn present (constitution ✓) and accurate, including the non-crypto note.

### 2. Attempt indexing — correct in both layers
- `dispatch.rs` `complete_with_retry`: `for attempt in 0..3` (0-based) passes `attempt + 1` → helper attempt 1, 2 → bands [500,1000], [1000,2000]. The delay is computed inside `if attempt < 2` (no wasted computation on the final attempt).
- `runtime/agent.rs` `run_turn_attempt`: `let mut attempt = 0u32` (line 358); `attempt += 1` (line 416) precedes the sleep, so the first turn-level retry passes 1 → [500,1000] — exactly what the new test asserts for `parked[2]`, and consistent with the updated `MAX_PROVIDER_TURN_ATTEMPTS` doc ("0.5–1s, 1–2s, 2–4s"). The task brief's "already-1-based attempt" is accurate in effect (post-increment it is the 1-based retry index).

### 3. Parked value = actual slept duration; lands on the right record
Both layers `sleep(delay_ms)` then `record_backoff_ms(delay_ms as u32)` — the parked value is the actual jittered duration (max 16000, safe u64→u32 cast). The turn-level park (agent.rs:446) uses `self.agent_loop.provider()` after the sleep; the next attempt's first `complete()` creates the record that takes the parked value at entry — the same parking discipline as the request level and parked prep. The take-at-entry race (provider swapped between park and next record, e.g. a user model switch mid-sleep) is pre-existing and accepted per the plan; notably the 429 fallback path never parks, so a fallback switch cannot orphan a park.

### 4. No behavior regressions
- 429: `complete_with_retry` returns `Err` immediately on `is_rate_limited()` (dispatch.rs:738) — no sleep, no park. `run_turn_attempt`: 429 → `try_429_fallback()` → `continue` (no sleep/park), or no alternate / fallback also 429'd → `attempt = MAX` → final failure (no sleep/park). Nothing slept on any 429 path. ✓
- Non-retryable: both layers fail fast (dispatch.rs:738, agent.rs:407-414 → `attempt = MAX`). ✓
- `MAX_PROVIDER_TURN_ATTEMPTS = 3` unchanged. ✓
- Retry-note format: "retrying in {:.1}s" with `delay_ms as f64 / 1000.0` — sub-second delays now show "0.8s" instead of the old integer division's "0s". The only producer is dispatch.rs:752; a repo-wide search found no consumer parsing the text (the frontend keys off the `retrying: true` flag per agent.rs:420-421; the only other "retrying in" match is a historical plan doc). No test asserts the old format. ✓
- No other fixed-backoff ladders remain: searches for `2u64.pow` / `delay_ms *= 2` hit only historical review docs. ✓

### 5. Tests — sound, orders and bands match the real flow
- `complete_with_retry_attributes_backoff_sleeps_to_next_attempt` (tests.rs:2553): confirmed `#[tokio::test(start_paused = true)]`; FlakyProvider `fail_times: 2` → exactly 2 parks → band assertions [500,1000] then [1000,2000] match the helper bands. ✓
- `run_turn_attempt_attributes_backoff_to_next_attempt` (runtime/agent.rs): `fail_times: 3` → turn 1 exhausts `complete_with_retry` (calls 1–3, request-level parks [500,1000] then [1000,2000]), fails, turn-level sleep parks [500,1000] (attempt=1 — the regression: previously attributed to nothing), turn 2 succeeds on call 4. Park order [req-1, req-2, turn-1] and `calls >= 4` both match the actual retry flow. ✓
- `retry_backoff_ms_stays_in_the_equal_jitter_band` (64 samples × attempts 1–3): sound. ✓
- `retry_backoff_ms_actually_jitters` (32 samples, ≥2 distinct): relies on the wall clock — flakiness assessed **negligible**: Rust's `SystemTime::now()` uses `GetSystemTimePreciseAsFileTime` on Windows (sub-µs) and ns-resolution clocks on macOS/Linux, so 32 consecutive calls overwhelmingly span multiple clock ticks; a pathologically frozen clock would fail loudly, not flake silently. Making it deterministic would require injecting a clock source into a jitter helper — not worth it. Accepted as-is.
- Tokio's paused clock does not affect `SystemTime::now()` (it only mocks tokio's Instant-based timers), so jitter stays real under `start_paused` and the band assertions are unaffected.

### 6. Constitution / platform / security
- Warning-free build: no unused imports/variables introduced (statically checked); the stated green run (`cargo test` 1936+16 passed, exit=0) under `#![deny(warnings)]` at both crate roots proves zero warnings. (Reviewer has no shell; relying on the stated run plus static inspection.)
- Doc comments: new pub fn documented. ✓
- Multi-platform: `SystemTime` is std and cross-platform; no new dependencies (Cargo.toml untouched); no `cfg(windows)` additions; frontend untouched. ✓
- Line endings: normal diff hunks, no whole-file rewrites. ✓
- Security: SystemTime nanos used only for retry desynchronization, documented as non-crypto — no misuse (not used for ids, tokens, or any security decision). ✓

### 7. Doc sync (beyond the findings)
The spec knowledge file is updated in-diff and accurately documents the shared helper, both parking sites, the bands, and all test names. `README.md:60-62` phase descriptions remain accurate with jitter. The three stale fixed-ladder mentions are findings L1–L3 above.

### 8. Untracked files
`.coding/plans/26c01541.md` (the plan) and the `.coding/backlog.jsonl` status flip ride along with the commit per the side-car policy — expected, no action.

### Observation (non-finding, out of scope)
The turn-level retry note ("retrying (attempt 1/3)") does not surface the jittered delay while the request-level note does ("retrying in 0.8s") — pre-existing (the old turn-level note also had no delay), unchanged by this plan; optional future nicety only.
