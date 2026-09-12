## Verdict: FINDINGS (0 high, 1 low)

**Summary:** The hysteresis compaction fix is correct, well-tested, and properly documented at the code level. The root cause is thoroughly recorded in the BUG knowledge file, and both required regression tests exercise the changed path. One low doc-sync finding: the BUG file's FIX section still reads "(pending)" and lists candidate mitigations rather than recording the implemented fix and regression-test names.

### What was reviewed

Uncommitted changes on `wt/agenticcoding` for bug_fixing plan 539e16c4:
- `src/agent/context.rs` — `compact_old_tool_results` signature change (added `keep_high`, returns `usize`), hysteresis no-op logic, two new regression tests, all existing tests updated to the 4-arg form.
- `src/agent/turn.rs` — call site updated to `(messages, 10, 20, 500)`; `token_accounting.reset()` now gated on `truncated > 0`.
- `.coding/knowledge/bug/2026-12-31-cache-hit-terrible-at-any-context-size-r14-per-b.md` — BUG knowledge file (root cause).
- `.coding/knowledge/how/2026-12-31-reportless-reviewer-failure-on-glm-retry-with-em.md` — unrelated HOW record bundled for commit.

### 1. Code correctness, edge cases, off-by-one

**Hysteresis boundary — correct.** `effective_high = keep_high.max(keep)` (= 20). No-op when `intact_indices.len() <= effective_high` (≤ 20); fires when `> effective_high` (≥ 21). This matches the doc ("once the intact window exceeds `keep_high`") and the design intent ("grow to ~20 then cut to 10"). The `<=` grants one extra stable batch at exactly 20; internally consistent — no off-by-one error.

**Slice arithmetic — no underflow.** `to_compact = &intact_indices[..intact_indices.len() - keep]` is reached only when `intact_indices.len() > effective_high >= keep`, so `len - keep ≥ 1`. No underflow. The slice selects the oldest `len - keep` intact results; the newest `keep` (the tail of `intact_indices`) are preserved. Correct.

**Idempotency — correct.** Truncated results gain `COMPACTED_MARKER` and are filtered out of `intact_indices` on the next pass, so a second call sees only the surviving intact window (≤ keep ≤ effective_high) and no-ops. Verified by `compact_is_idempotent` and the second-call assertion in `compact_fires_at_high_water_mark`.

**Short-result skip — graceful, not a regression.** Results shorter than `summary_chars + 100` are skipped (no marker, no count). Such results stay "intact" forever and could in principle let the intact count grow unboundedly, re-triggering the fire branch every batch while truncating nothing (returns 0, no reset, no mutation → cache stays stable). This is pre-existing behavior (the short-skip predates this change) and degrades safely: no mutation means cache stability holds, and the higher-level summarization threshold eventually compacts everything. Not a finding.

**Return-value / reset gating — correct.** `truncated_count` is incremented only on `msg.content` reassignment, so `truncated > 0` iff at least one message was mutated. `token_accounting.reset()` now fires only on actual mutation; when the function no-ops (the common case), the incremental accounting stays valid and avoids a needless full re-count. Correct.

**Edge cases:** empty slice → `0 <= 20` → return 0 (safe). `keep_high < keep` → `effective_high = keep` → falls back to the original "cut to keep" behavior (safe, though not a real call site). `keep = 0` → truncates all intact (degenerate, not a real call site).

### 2. Bugs and security

No bugs found. No security concerns: the function operates on local in-memory message content; no external input parsing, path handling, or injection surface. `take(summary_chars)` and `contains(COMPACTED_MARKER)` are char/byte-safe (no panics on the multibyte `…`). The code graph confirms all 6 callers (5 tests + `run_turn`) are updated to the new signature; no stale 3-arg call remains, so the build is consistent. The `Message::text` / `Message::tool_result` constructors take `impl Into<String>`, so the new tests' owned-`String` arguments compile.

### 3. Multi-platform, docs, build, line endings

- **Multi-platform neutrality:** Pure Rust logic — no platform APIs, paths, shell, or `cfg(windows)`. Neutral on macOS and Windows. ✓
- **Doc comments:** `compact_old_tool_results` has a thorough updated doc comment (hysteresis rationale, prefix-cache impact, return-value contract, caller reset guidance). The `turn.rs` call site has an updated inline comment. ✓
- **Warning-free build (by inspection):** No unused variables (`effective_high`, `truncated_count` both used), no dead code, no `#[allow]`. Under `#![deny(warnings)]` a clean compile proves zero warnings; the code is consistent and all call sites are updated. (Could not run `cargo test` as a read-only reviewer, but the change is compile-consistent.)
- **Line endings:** No CRLF/LF mixing evident in the diff; new lines match surrounding LF style. ✓

### 4. Bug-plan specific checks

- **Regression tests exist and exercise the changed path — ✓.**
  - `compact_below_high_water_mark_mutates_nothing` (14 results, keep=10, keep_high=20): asserts `truncated == 0` and byte-for-byte message equality after the call. This directly exercises the new no-op branch — the core of the fix. Against the old (no high-water mark) behavior it would have truncated 4 results, so it fails without the fix and passes with it.
  - `compact_fires_at_high_water_mark` (21 results, keep=10, keep_high=20): asserts `truncated == 11`, the correct 11 oldest truncated / 10 newest intact, and a second call returns 0 (idempotent). Exercises the fire branch and the intact-filtering.
- **Root cause documented — ✓.** The BUG file states: "Each batch slides the keep-window; the result falling out is mutated → the next request differs from the previous one at that message → the longest-common-prefix cache breaks there" and "newly truncated result #7 … the first byte-difference vs the previous request." This matches the required framing (sliding-window in-place compaction moved the first byte-difference every request), with empirical traces.jsonl confirmation.
- **BUG knowledge file written at the required path — ✓.** Present at `.coding/knowledge/bug/2026-12-31-cache-hit-terrible-at-any-context-size-r14-per-b.md`.

### Findings

**LOW — BUG knowledge file FIX section is stale ("(pending)").** The file's `FIX (pending)` block still lists three candidate mitigations (a/b/c) and does not record that option (a) — the hysteresis high-water mark (no-op ≤ 20, one cut to 10) — was the implemented fix, nor the regression-test names (`compact_below_high_water_mark_mutates_nothing`, `compact_fires_at_high_water_mark`). Per the project's BUG-record convention (symptom → root cause → fix + regression test name) and the documentation-sync review expectation, update the file to close the loop: mark the fix as implemented (option a) and name the regression tests. The symptom and root-cause sections are complete and accurate; only the fix section needs finalizing.

### Non-blocking observations

- Test name `compact_fires_at_high_water_mark` fires at `keep_high + 1` (21), i.e. when the mark is *exceeded*, not at exactly 20. The test body and doc are correct (exclusive boundary); the name is slightly loose but not misleading enough to warrant a rename.
- The unrelated HOW record `2026-12-31-reportless-reviewer-failure-on-glm-retry-with-em.md` is bundled into this commit. Knowledge files legitimately travel with git, so this is fine — noted only for commit-hygiene awareness.
