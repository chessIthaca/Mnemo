## Verdict: PASS

Round-2 verification for plan 26c01541 "Retry backoff: equal jitter + turn-level trace attribution" (wt/agenticcoding, HEAD = a1da008, clean tree — `git diff HEAD` and `git status --short` both empty). All three round-1 findings are fixed correctly and accurately at HEAD; the fix commit introduces no new issues; every spot-checked round-1 claim still holds. The plan is complete — this is the final gate.

## 1. The three round-1 fixes — present and accurate

**L1 — `src/agent/dispatch.rs:674-676` (`complete_with_retry` doc)** ✓
Now reads: "Call `provider.complete` with retry + jittered exponential backoff. / Retries up to 3 times on provider errors, with equal jitter (~0.5–1s then ~1–2s — see [`crate::error::retry_backoff_ms`])." Accurate on every count, checked against the live code:
- Bands: `retry_backoff_ms` (src/error.rs:224-233) computes `nominal = 1000 << (attempt-1).min(4)`, `half = nominal/2`, `delay = half + nanos % (half+1)` → inclusive [half, nominal]: attempt 1 → [500,1000] ms, attempt 2 → [1000,2000] ms. "~0.5–1s then ~1–2s" ✓.
- The unreachable "4s" rung is correctly gone: `for attempt in 0..3` with the `if attempt < 2` guard (dispatch.rs:742) calls the helper with `attempt + 1` ∈ {1, 2} only — at most 2 sleeps, max 2000 ms.
- The intra-doc link `[crate::error::retry_backoff_ms]` resolves (pub fn, error.rs:224).
- The adjacent updated line "up to ~3s of jittered retry sleeps" (dispatch.rs:683) is also accurate: worst case 1000 + 2000 = 3000 ms.

**L2 — `src/agent/dispatch.rs:12-14` (module header)** ✓
Now reads: "`complete_with_retry` wraps the provider's streaming `complete` with jittered exponential backoff (equal jitter — see `retry_backoff_ms`) so a transient gateway error doesn't kill the turn." Matches the round-1 suggested fix; accurate.

**L3 — `PLAN.md:374-376` (retry design)** ✓
Now reads: "Provider errors retry with jittered exponential backoff (equal jitter via `retry_backoff_ms`: ~0.5–1s, ~1–2s) so concurrent agents retrying the same recovering endpoint don't stay in lockstep." Matches the round-1 suggested fix; accurate — both layers' reachable rungs, and the lockstep rationale is the helper's actual documented purpose.

## 2. No new issues from the fix commit (a1da008)

a1da008 is the single commit landing the whole plan: the implementation was reviewed uncommitted in round 1, the three doc fixes were folded into the working tree, and everything committed together — 9 files: `src/agent/dispatch.rs`, `src/agent/tests.rs`, `src/error.rs`, `src/runtime/agent.rs`, `PLAN.md`, the spec knowledge file, the plan file (`.coding/plans/26c01541.md`, new), the round-1 report (new), and the `.coding/backlog.jsonl` status flip. The delta over the round-1-reviewed tree is exactly the three doc comments plus the side-car additions; everything else matches what round 1 verified.

- **Backlog flip**: item 40bc1a65 (trace-graph phase attribution, plan 6e734ccb) `in_flight` → `done`, note listing commits 28cd957, 4ddda37, 835fdf6 + round-3 PASS — verified against git log (exactly the 6e734ccb commits); the stale "Aborting turn" note is replaced with the accurate completion note. Rides along per the side-car policy, as the task brief states.
- **Spec file** (`.coding/knowledge/spec/2026-12-30-trace-phase-attribution-connect-backoff-stall-sp.md`): already in the reviewed tree (round-1 §7); its updated text is accurate — the bands, both parking sites (dispatch.rs request-level + agent.rs turn-level), and every named test match the code.
- **Plan file**: all 5 steps checked off, matches what shipped.
- The fix delta is comment/prose-only — nothing that can change compilation or test outcomes.

## 3. Round-1 core claims — spot-checked, all hold at HEAD

- **Helper** (error.rs:224-233): equal jitter verified — attempt 1 → [500,1000], 2 → [1000,2000], 3 → [2000,4000]; shift capped at 4 (nominal max 16000, no overflow); `saturating_sub` makes attempt=0 degrade to band 1; `unwrap_or(0)` clock-skew fallback; pub fn with an accurate doc comment including the non-crypto note.
- **Attempt indexing**: dispatch.rs:747 `retry_backoff_ms(attempt + 1)` (0-based loop, inside `if attempt < 2`); agent.rs:416 `attempt += 1` precedes agent.rs:437 `retry_backoff_ms(attempt)` (post-increment 1-based — first turn-level retry → band 1). Both correct.
- **Parked value = actual slept duration**: both layers `sleep(Duration::from_millis(delay_ms))` then `record_backoff_ms(delay_ms as u32)` (dispatch.rs:761-769; agent.rs:438-446 via `self.agent_loop.provider()`); max 16000 fits u32.
- **429 / non-retryable neither sleep nor park**: dispatch.rs:739-741 returns `Err` before the sleep block; agent.rs no-alternate-429 and non-retryable set `attempt = MAX` (lines 403-414), skipping the `if attempt < MAX` sleep block (line 418); the 429-with-alternate path `continue`s at line 400 without sleeping or parking. ✓
- **The four regression tests** — all present at HEAD and matching the real retry flow:
  - `complete_with_retry_attributes_backoff_sleeps_to_next_attempt` (tests.rs:2596-2606): `parked.len() == 2`, band asserts [500,1000] then [1000,2000]. ✓
  - `run_turn_attempt_attributes_backoff_to_next_attempt` (agent.rs:4608-4627): `parked.len() == 3`, park order [req-1 (500-1000), req-2 (1000-2000), turn-1 (500-1000)], `calls >= 4` — exactly the flow with `fail_times: 3` (turn 1 exhausts `complete_with_retry` with 2 request-level parks, the turn-level sleep parks the 3rd, turn 2 succeeds on call 4). ✓
  - `retry_backoff_ms_stays_in_the_equal_jitter_band` (error.rs:240-252): 64 iterations × attempts 1-3, inclusive band asserts. ✓
  - `retry_backoff_ms_actually_jitters` (error.rs:255-265): 32 samples, ≥2 distinct. ✓
- **No stale fixed ladders remain**: repo-wide search for "1s, 2s, 4s" hits only historical point-in-time records (the 2026-09-15 performance review, the round-1 report quoting the old text, the PRD review, root `reviews/02-quality.md`) — all deliberately kept per round 1's decision; no live doc (README, PLAN.md, module docs, spec) carries the ladder. `2u64.pow` / `delay_ms *= 2` hit only historical review docs. `README.md:61` "backoff (retry sleeps before this attempt)" remains accurate with jitter.

## 4. Test run

The commit message records `cargo test` green after the fixes: 1936 + 16 passed, exit=0. The reviewer has no shell (same as round 1) — relying on the stated run plus static inspection. The fix delta is docs-only (doc comments + PLAN.md prose), so it cannot affect test outcomes; the four regression tests are byte-identical to the round-1-reviewed state. Under `#![deny(warnings)]` at both crate roots, the stated green run also proves the build is warning-free.

## Observations (non-findings)

- `MAX_PROVIDER_TURN_ATTEMPTS`'s doc (agent.rs:21-26) enumerates the helper's full band schedule "(0.5–1s, 1–2s, 2–4s)" while the turn layer sleeps at most twice (once `attempt` reaches 3, `if attempt < MAX` is false), so the 2–4s rung never fires at the turn level. Not a finding: it is part of the round-1-reviewed implementation (not the fix delta), was explicitly examined and accepted in round 1 (its §2), and reads as the shared helper's per-attempt schedule with a "see `retry_backoff_ms`" pointer — the helper genuinely defines those bands for any attempt. The same applies to the "3×~1s/2s" shorthand in the two 429 comments (dispatch.rs:712, agent.rs:412) — colloquial "the retry ladder" phrasing carried over from the pre-plan text and reviewed in round 1.
- Round-1's own observation stands unchanged: the turn-level retry note ("retrying (attempt 1/3)") does not surface the jittered delay while the request-level note does — pre-existing, out of scope, optional future nicety only.

## Conclusion

All three round-1 findings are fixed correctly and accurately; the fix commit introduces no new issues; every spot-checked round-1 claim holds at HEAD; the stated test run is green (1936 + 16, exit=0). Plan 26c01541 is complete — final gate passed.
