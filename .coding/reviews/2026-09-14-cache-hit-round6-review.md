## Verdict: FINDINGS (1 high, 3 low)

**Scope.** All uncommitted changes on `wt/mnemo` for plan c43ad4a3 (repo root per `git status`: the Mnemo app dir): `src/agent/context.rs` (+97/−3 — the `tool_result_is_truncatable` helper, the gate rewrite in `compact_old_tool_results`, regression test `short_results_do_not_hold_the_gate_open`), plus untracked `.coding/analysis/cache-hit-6-{extract.py,aggregates.txt,report.md}`, `.coding/knowledge/bug/2027-01-11-compaction-gate-permanently-open-a-sent-tool-res.md`, and the two plan records (app bookkeeping, not reviewed as code).

**Summary.** The source fix is correct and the regression test is a genuine guard; the analysis artifacts are honest. One structural doc defect must be fixed before commit (HIGH-1), plus three low findings confined to `.coding/analysis/`.

---

## The crux checks out

**1. The helper mirrors `truncate_tool_result_at`'s refusal rules EXACTLY** (`src/agent/context.rs:843-883` vs `949-971`):
- **Text:** the function refuses iff `s.contains(COMPACTED_MARKER) || s.chars().count() <= keep_chars + 100` (849-851); the helper asserts truncatable iff `!contains && chars > keep+100` (951-953) — an exact De Morgan negation, same char-count metric (not bytes).
- **Parts:** the function refuses iff there is no `ImageUrl` part (861-866), then refuses iff the combined non-image text contains the marker (874-876); the helper runs the same two checks in the same order with the identical `filter_map`/`collect` construction (954-969). Neither applies the length rule to parts — correct, the payload IS the size (comment at 855-860).
- **Same `keep_chars`:** the gate passes `summary_chars` to both the helper (line 1011) and `truncate_tool_result_at` (line 1019). I found no divergence in either direction.

**2. Gate semantics are right.** `effective_high = keep_high.max(keep)` (979) is ≥ `keep`, and the gate fires only when `truncatable.len() > effective_high`, so `truncatable_indices.len() - keep` cannot underflow — the same invariant the old intact-count code had. `to_compact` is now the right population: a short result can never be compacted (refused, unmarked), so it was never itself a rewrite hazard — it only poisoned the count. Protecting the newest `keep` *truncatable* results is exactly the cache-relevant window.
- **`enforce_sendable` / `mechanical_compaction` (keep=0, keep_high=0):** effective_high=0 → the gate fires iff anything is truncatable, and `to_compact` = all truncatable indices. Byte-identical OUTCOME to the old code (which passed every intact index to the function and had the shorts refused inside it), just without the wasted calls. The aggressive pass truncates everything it can, as before.
- **Common all-long history:** truncatable == intact → behavior identical to pre-fix (same threshold, same cut). The only changed case is a mixed history with ≥ `keep_high - keep + 1` unmarkable shorts — precisely the defect.
- **Return value** semantics unchanged (count of newly truncated), so the callers' reset-token-accounting rule is unaffected. Callers verified via the code graph: `execute_tool_batch` (`src/agent/turn.rs`, the `(10, 20, 500)` production site), `mechanical_compaction` (1094, `(0, 0, AGGRESSIVE)`), `enforce_sendable`, and 14 tests — all covered by the two cases above.

**3. The regression test is a real guard encoding the actual defect.** 2 long (5,000-char) results FIRST, then 24 short (~216-char, ≤ 601) results → 26 intact. Pre-fix arithmetic: intact 26 > effective_high 20 → `to_compact = intact[..26-10]` = the first 16 intact, which reach the 2 long results (messages 2-3) → `truncated = 2` → `assert_eq!(truncated, 0)` fails with exactly the reported "left: 2, right: 0", and the content asserts fire too. Post-fix: truncatable = 2 ≤ 20 → strict no-op; both asserts hold. The long-first placement is load-bearing (long results last would leave the pre-fix cut entirely inside the shorts and the test would pass vacuously) and the test comment says so — it encodes the defect, not a coincidence. The oversize pass is inert in the test (`TOOL_RESULT_MAX_CHARS = 100_000`, context.rs:788, ≫ 5,000), so the test exercises only the hysteresis gate. I verified this statically; the reported unpiped `cargo test` (2329 passed / 0 failed / 5 ignored, one more than before) is consistent, and under `#![deny(warnings)]` the green suite also proves warning-freedom. The new code has no unused items, the matches are exhaustive, and there is no borrow tangle (indices are collected before the mutable truncation pass).

**4. The analysis artifacts are honest.** The extractor is read-only (sqlite `mode=ro`, traces read line-wise) and writes only its own aggregates file — the sanctioned GENERATED-artifact pattern matching the round-5 precedent; no shell mutation anywhere in the change set. Rates are tagged M vs E; the 13-record/1-session sample carries an explicit SAMPLE WARNING in section A, and the report's "Sample honesty" paragraph routes every rate claim to `request_stats`. B2's chars/4 figures are labeled ESTIMATE and calibrated against provider `prompt_tokens`; B3's audit table reproduces the pre-fix code's arithmetic correctly (intact 68-71 > 20 on every request, 60-63 permanently-unmarkable shorts, 7-8 truncatable). The "NOT the cause" claims are supported by the aggregates: 320-360K bin 96.9% (n=5, disclosed in the histogram), all six models report cache, hysteresis exists but is defeated. The ranking is defensible: R6-1 is contained, mechanism measured, test fails without the fix; R6-2 reverses documented, test-pinned intent (`schema_filter_research_plan_uses_executing_research`), so escalating it to a user decision instead of silently changing it is right; R6-3 is correctly priced at zero hit-rate effect. I found no overclaim from the 13-record sample: every mechanism claim (MID 5/5, byte offsets 504-517, B2 YES on 3→4/4→5, 36-41K re-billed) traces to an aggregates section, and the ~99% expected win is qualified as the measured stable-prefix rows (section F: 99.0-99.7%). The bug knowledge record's claims all check out against code and aggregates.
---

## Findings

### HIGH-1 — `compact_old_tool_results` lost its doc comment; the helper inherited it

The helper was inserted between the function's doc block and its signature. Lines 885-948 are ONE contiguous `///` block — line 938 ("…so idempotency is unaffected.") runs straight into line 939 ("Whether [`truncate_tool_result_at`] would actually shrink this message.") with no blank line — so all 64 lines, including the "# Hysteresis semantics and prefix caching" and "# Re-read pointers (D2)" sections, now attach to `fn tool_result_is_truncatable` (949), and `pub fn compact_old_tool_results` (973) is left with **no doc comment at all**. Consequences:

- Violates agent.md's hard rule "All public functions must have doc comments" — on the exact public function this plan changed.
- rustdoc renders the pub function's entire contract (hysteresis window, oversize pass, re-read pointers) on a private one-liner helper; no warning fires because `missing_docs` is allow-by-default, so the green `cargo test` does not catch it.
- The diff confirms the severance: the inserted `+///` lines sit directly after the doc tail context line and directly before `pub fn compact_old_tool_results(`.

**Fix:** move the helper (with its own short doc) ABOVE line 885 or BELOW the end of `compact_old_tool_results`, keeping the 885-938 doc block contiguous with its function.

**Folded into the same fix** — the re-attached block's hysteresis paragraph (896-901) is stale: it still says the gate counts "intact (un-truncated) tool results" and cuts "all but the newest `keep` intact results", contradicting both the new code and the accurate inline comment at 999-1007. Rewrite it to the truncatable-population semantics while relocating it. Minor while there: the helper's doc omits the multipart marker rule it implements (line 968) — worth half a sentence.

### LOW-1 — extractor B2 calibration: the intra-message byte-offset refinement is dead code

`cache-hit-6-extract.py:355` guards the refinement with `if k < len(prev) and k < len(cm):` — but `prev` is the trace RECORD dict (~10 top-level keys), not the message list `pm`; with k ≈ 271-306 the condition is always false, so `chars_before += j` never executes and `pre` under-counts the ~504-517-byte partial match inside the mutated message (~325 tokens at the calibrated ratio — immaterial against the `max(0.1 * prompt, 4000)` tolerance at line 370, no verdict flips). Should be `len(pm)`. Related bias worth a note: `pre`'s numerator omits the tools-array chars that the calibration denominator (line 350) includes; that systematic ~+13-16K underestimate of the gap matches the "+15,699 / +14,361 above pre-break" NO verdicts on pairs 1→2 / 2→3 (the 30-34-tool arrays are about that size). The report cites only the YES pairs (3→4 / 4→5), so no conclusion rests on the NO rows — but those two NO rows are most likely estimator artifacts, not provider behavior.

### LOW-2 — report ↔ aggregates snapshot drift

The report cites `request_stats` "972 rows" / headline 966 rows 79.1% / deepseek-v4.1-flash n=87 63.0% / traced window 24 rows 62.6%, but the final `cache-hit-6-aggregates.txt` — the artifact the report itself points to — says 1006 rows / 1000 main-loop rows / 79.7% / v4.1 n=121 74.8% / F-window 25 rows 63.5%. The DB kept accumulating between the report's authoring and the final extractor run (the traced session's rows plus the analysis session's own requests). No conclusion changes — both runs show the same shape — but the report's numbers no longer cross-check against their cited artifact. Either refresh the table from the final run or add a one-line "snapshot at report time" note.

### LOW-3 — extractor section bookkeeping

The script emits the filled "E. mutation-path cross-check verdicts" (hardcoded `w()` literals, script lines 552-595) between C and F, AND still runs the original "(filled by the code cross-check step) / (pending)" placeholder block (script lines 630-633) at the end — so the aggregates file carries two "E." headers, the second empty (output lines 173 vs 249). Also: the module docstring calls the cross-check slot "D" (line 21) while the output labels it "E", and the `# ── C. request_stats ──` comment banner (script line 378) actually wraps the B3 gate audit. Delete the dead placeholder and align the labels — this is a re-runnable instrument. (Sub-nit, immaterial direction: B3 classifies "short" by canonicalized-JSON length ≤ 601 — which adds quote/escape chars vs the raw `chars().count()` the Rust rule uses — and ignores the multipart no-image rule; ±1-row effects at most, and the error over-counts truncatable, i.e. understates the bug.)

---

## Checked and clean

- **Multi-platform neutrality:** pure Rust string/index arithmetic — no `cfg`, no paths, no shell; the extractor is stdlib-only with relative paths and a read-only sqlite URI. Fine on macOS and Windows.
- **File-tools-first:** no shell mutation anywhere in the change set; the aggregates `.txt` is script-generated — the accepted pattern for GENERATED artifacts (round-5 precedent).
- **Documentation sync:** no README/PLAN.md update owed — internal compaction mechanics, no user-facing surface; the bug knowledge record IS the documentation and is accurate (verified against code + aggregates). The one doc defect is HIGH-1.
- **Security:** nothing new — no untrusted-input handling changes; the extractor's SQL is a fixed SELECT opened read-only.
- **Line endings / style:** additions only, no EOL churn; test style matches its siblings; the new helper is private with a doc comment (content aside).

## Notes for the fixer

- HIGH-1 moves ~35 lines with no code change; re-run `cargo test` after (expect the same 2329) — nothing depends on the doc block's position.
- LOW-1..3 touch only `.coding/analysis/` files; LOW-2 can be a one-line note rather than a regeneration.