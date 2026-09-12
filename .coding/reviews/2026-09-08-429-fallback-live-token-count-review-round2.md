## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of commit 739c68a (HEAD of wt/agenticcoding, clean tree — `git status`/`git diff HEAD` empty) — the complete 429-fallback live-token-count change set (fix + tests + docs + round-1 report). **All three round-1 findings are verified ACTUALLY resolved in the code**, at the prescribed sites, with the prescribed semantics; the core fix and its four regression tests are intact; the fixes introduced no code regressions. One new LOW: the committed knowledge file (the durable BUG record) still describes the pre-HIGH-1 headroom-only rule — the HIGH-1 code fix landed without syncing it.

**Method.** Every claim below verified against the actual code at 739c68a: the full commit diff (`git show 739c68a`), the viability check + field/method docs (src/agent/loop_impl.rs), the store site (src/agent/turn.rs), all four regression tests + test doubles (src/runtime/agent.rs), README.md, the factory wiring (src/agent/factory.rs), and the committed knowledge file. The claimed green `cargo test --workspace` (2479/0) and the red-on-headroom-only history of the high-fill test are trusted per the task brief and sanity-checked statically (margins recomputed below; no unused imports, no `#[allow]`, doc comments present — nothing that would trip `#![deny(warnings)]`).

## Round-1 finding resolution

### HIGH-1 (headroom rule regresses failover for fill_rate > 0.5) — RESOLVED

The fix landed exactly as prescribed — src/agent/loop_impl.rs:1728-1743:

```rust
let alt_cm = resolver.build_turn_provider(&alt, self.fill_rate)?.1;
let cur_window = self.context_manager.read().expect("context_manager lock poisoned").max_tokens();
let live = self.live_token_count.load(std::sync::atomic::Ordering::Relaxed);
let viable = if live > 0 {
    alt_cm.max_tokens() >= live + alt_cm.effective_summarize_at()
        || alt_cm.max_tokens() >= cur_window
} else {
    alt_cm.max_tokens() >= cur_window
};
if !viable { return None; }
```

- `cur_window` hoisted (:1729-1733); the OR (:1737-1738) makes the live>0 rule **strictly dominant over the pre-fix acceptance set for any fill rate**: the window branch `alt ≥ cur_window` is exactly the old rule, and the headroom branch only adds acceptances (the bug's target case — mixed-window, live fits with headroom). The live==0 branch (:1739-1740) is semantically identical to the old rule. The HIGH-1 regression slice (alt ≥ cur AND live > alt×(1−fill)) is closed by construction, not just in the tested configuration.
- Sticky-entry semantics unchanged: `if !viable { return None; }` (:1741-1743) still precedes the sticky insert.
- Docs updated with the fix: method doc (loop_impl.rs:1672-1679 — "can neither hold the live conversation plus headroom (its own summarize threshold) nor match the current endpoint's window") and the inline comment (:1704-1727), which derives the fill>0.5 bar-sits-below-the-incumbent's-band argument and states "the OR keeps every pre-fix acceptance (strictly dominant)".

The regression test is real and squarely in the regression slice — `rate_limit_429_high_fill_rate_does_not_regress_window_viability` (src/runtime/agent.rs:2135-2263): fill 0.75 via `.with_fill_rate(0.75)` (:2192 — the only source of the loop's fill rate; `from_config` hardcodes 0.5 at loop_impl.rs:720, and the app factory passes the configured rate at factory.rs:767 inside `build_inner`), current window 128k (config CM :2185 + primary caps :2162), alternate 200k (:2169), live ~60k ("a b " × 30k ≈ 2 cl100k_base BPE tokens per repeat, :2200-2203). Recomputed margins: the headroom branch rejects at 60k + 150k = 210k > 200k (10k margin — the test cannot pass via the headroom branch); the OR's window branch accepts at 200k ≥ 128k (72k margin); no compaction interferes (60k < 96k current threshold, 35k margin). Asserts primary called once, the alternate serves the recovery (≥1), switching note, Finished, no terminal error — without the OR the test fails exactly as the round-1 finding describes. (Red-on-headroom-only trusted per the brief; statically the test can only pass through the OR branch.)

### LOW-1 (doc precision — the stored count's basis) — RESOLVED

Both prescribed sites now state the basis:
- Field doc (src/agent/loop_impl.rs:104-114): "The basis is the harness's canonical live count (the same one the summarize trigger uses): messages + tools-schema overhead, EXCLUDING the per-request system scaffolding (the stable head on the first iteration, the volatile tail + CONTEXT_FOOTER on every iteration) — the viability headroom absorbs that scaffolding and the alternate's own preflight/compaction is the backstop."
- Store-site comment (src/agent/turn.rs:316-323): "Basis: messages + tools overhead — the per-request head/tail scaffolding is installed after this point and is absorbed by the viability headroom (the alternate's preflight is the backstop)."

Both carry the round-1 suggested content (basis + absorption + backstop); no code change was made, as prescribed.

### LOW-2 (README stale clause) — RESOLVED

README.md:60 now reads "...surfacing an actionable \"no alternate provider found\" error only when no other endpoint serves the model **or none can hold the live conversation**" — exactly the required clause, in the single 429 sentence (the only README mention of the fallback).

## Core-fix spot-checks (all intact)

- **Store site** — turn.rs:314 (`token_accounting.update`) → :324-325 (`live_token_count.store(token_count, Relaxed)`), before `maybe_compact` and the request; init `AtomicUsize::new(0)` in `from_config` (loop_impl.rs:704 — the single constructor body). Relaxed ordering unchanged and correct (same-task await chain).
- **Four regression tests** — (a) mixed-window failover on small live, the original verified-red reproduce (runtime/agent.rs:1794-1922; the headroom branch accepts tiny + 64k ≤ 128k where the window branch alone would reject); (b) genuine-overflow rejection (:1925-2052; ~80k live vs the 64k viability line — both branches reject: 80.5k + 64k > 128k and 128k < 200k; asserts terminal "no alternate provider", zero alternate calls); (c) live=0 window-fallback unit test (:2055-2132; None at live=0, Some("primary") at live=20k); (d) the HIGH-1 high-fill regression (:2135-2263, above).
- **Test doubles** — `FallbackTestProvider` caps field (runtime/agent.rs:1427) + `fallback_test_caps` helper (:1434-1444, doc comment :1430-1433); exactly 16 pre-existing construction sites updated in the diff (hunks at :1572/:1582, :1696/:1706, :2288/:2298, :2404/:2414, :2636/:2646/:2653, :2832/:2842/:2849, :3021/:3031), all `fallback_test_caps(128_000)` — field-for-field identical to the removed `const C`; 8 new sites across the four new tests (24 call sites total, matches a fresh index search).
- **No collateral production changes** — the runtime/agent.rs diff is entirely inside `mod tests`; the production caller (`run_turn_attempt`) is untouched from round-1's review. Frontend untouched (no frontend/ files in the commit — no npm run needed). Multi-platform neutral: pure core Rust, no cfg(windows), no paths, no platform APIs.
- **Overflow/lock hygiene re-checked on the new shape** — `live + effective_summarize_at()` ≤ ~11M ≪ usize::MAX; the `cur_window` read guard drops at the statement end, no lock held across the sticky insert.

## New issue introduced by the fixes

### LOW-3: the committed knowledge file still describes the pre-HIGH-1 rule (stale docs)

.coding/knowledge/bug/2027-01-07-429-fallback-viability-compared-context-windows.md:10 (Fix paragraph, committed new in 739c68a): "viability = alt_window >= live + alt_cm.effective_summarize_at() (...) Window comparison kept as the fallback when live == 0. Regression tests: the mixed-window failover test (fails pre-fix), the genuine-overflow reject test, and a direct unit test for the live=0 window-fallback path."

That is the round-1 (pre-HIGH-1-fix) shape: it omits the OR'd window comparison (`|| alt_window >= cur_window`, loop_impl.rs:1737-1738) and lists 3 of the 4 regression tests (missing `rate_limit_429_high_fill_rate_does_not_regress_window_viability`). The round-1 review verified this file "matches the shipped code" — true then; the HIGH-1 fix changed the code (loop_impl.rs:1735-1740) without syncing the file. This file is the durable BUG record the derived memory (BUG: 429-fallback viability compared context windows…, id 1e5f96b1) points at — a future reader implementing "the fix" from it would reintroduce the HIGH-1 regression the OR exists to prevent. No runtime impact (docs only) → LOW per the project's documentation-sync review expectation. Suggested fix: one-sentence update to the Fix paragraph ("… OR alt_window >= the current window — keeps every pre-fix acceptance at any fill rate (review HIGH-1)") + name the fourth test; supersede/refresh the derived BUG memory record alongside.

(For completeness: the committed plan file de1f874b.md also describes the pre-OR rule in its detailed steps, but plans are point-in-time planning artifacts like review reports — not living documentation of the shipped state; not a finding.)

## Bottom line

All three round-1 findings are genuinely resolved at the exact prescribed sites with the prescribed semantics: the OR restores strict dominance over the pre-fix acceptance set (HIGH-1, with a real-margin regression test that can only pass through the OR branch), and both doc fixes (LOW-1 basis, LOW-2 README clause) are in place verbatim. The core fix, its four regression tests, and the test-double refactor are intact, and nothing in the production code regressed. The one residual is documentation: the durable bug-record file (and the memory record derived from it) still describes the superseded headroom-only rule — a one-sentence sync away from a fully clean change.
