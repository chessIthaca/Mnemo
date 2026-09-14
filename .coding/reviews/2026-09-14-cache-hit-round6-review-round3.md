## Verdict: PASS

**Scope.** Round-3 (closing) verification of commit `0aed033` (HEAD of `wt/mnemo`, working tree clean — `git diff HEAD` and `git status --short` both empty) for plan c43ad4a3 (cache-hit round 6). Round 2 (`.coding/reviews/2026-09-14-cache-hit-round6-review-round2.md`) returned FINDINGS (0 high, 3 low) on top of round 1's closed 1 high / 3 low; all three round-2 findings were fixed in `0aed033`. This round verifies those three fixes are real and complete, that nothing else moved, and closes documentation/artifact consistency. The substantive fix (`f1a5809`, verified failing-with-the-fix-neutered via `short_results_do_not_hold_the_gate_open`) and the B2-guard round-2 verification are NOT re-litigated. All three fixes verify; no new findings.

**Commit shape.** Stat: exactly the five claimed files — `src/agent/context.rs`, `.coding/analysis/cache-hit-6-extract.py`, `.coding/analysis/cache-hit-6-aggregates.txt`, `.coding/analysis/cache-hit-6-report.md`, `.coding/reviews/2026-09-14-cache-hit-round6-review-round2.md` — "5 files changed, 138 insertions(+), 31 deletions(-)" as claimed; nothing unexpected (the round-2 review report itself is one of the claimed five). Parent is `f1a5809` (`git log` confirms HEAD `0aed033` → parent `f1a5809`). **The src delta is comment-only:** the only `context.rs` hunk touches lines 926-928, three `///` doc-comment lines (one line replaced by two); no code token changes, no new imports/items, the regression test is untouched.

---

## Finding 1 — FIXED: rule-1 sentence now states the truncatable gate

`src/agent/context.rs:926-928` now reads:

> 1. The hysteresis pass above — cut results back to `summary_chars` once the TRUNCATABLE population passes the high-water mark. Byte-identical while under it.

Exactly the prescribed wording. Consistency verified on all three required anchors:

- **(a) The rewritten hysteresis section (896-911)** says the same thing: "a hysteresis high-water mark over the TRUNCATABLE population", "while that count is `<= keep_high`, this function is a strict no-op", "Once it exceeds `keep_high`, it cuts all but the newest `keep` truncatable results back to `summary_chars` plus [`COMPACTED_MARKER`]". Rule 1 now mirrors it; the false implication (intact > high ⟹ a cut) is gone.
- **(b) The code:** the gate is `if truncatable_indices.len() > effective_high` (line 992; `effective_high = keep_high.max(keep)` at 956) on the truncatable population collected at 985-989 by filtering the intact list through `tool_result_is_truncatable`.
- **(c) The B3 audit:** intact 68-71 > keep_high 20 on every request while truncatable is only 7-8 — under the fixed gate the same session no-ops; the new sentence is consistent with that data rather than contradicting it.

**Sweep for other false-direction sentences — clean.** The rest of the doc block (929-936: rule 2, the oversize cut of intact Text results > `TOOL_RESULT_MAX_CHARS`, explicitly "EVEN when the hysteresis gate was a no-op" — a correct statement about the oversize rule, not the gate trigger; 938-949: re-read pointers, no gate claims) and every inline comment (957-960 intact-collection description; 976-984 gate comment + correcting paragraph; 993 "Cut back to `keep` truncatable tool results"; 1002-1005 oversize scan rationale) contain no other sentence asserting that intact > high causes a cut. The helper doc (1031-1042) explains the truncatable-gate rationale consistently.

**Round-2 judgment on the inline comment re-confirmed, not re-flagged:** lines 976-977 ("no-op while the intact window is below or at the high-water mark") are one-directional and true as stated — intact ≤ high ⟹ truncatable ⊆ intact ≤ high ⟹ no-op — and are immediately corrected by the 980-984 paragraph ("The gate counts the TRUNCATABLE population, not the intact one"). The comment asserts no converse, so it stands.
---

## Finding 2 — FIXED: report numbers match the committed artifact

Verified `.coding/analysis/cache-hit-6-report.md` line-by-line against the committed `.coding/analysis/cache-hit-6-aggregates.txt`:

- **Instruments (report 4-7):** now cites `request_stats` "(1,064 rows, 22 sessions, 2026-09-12..14 — the size at the snapshot below; the table grows while the report is read)" — matches the artifact's `rows: 1064` (120) and `distinct sessions: 22` (123), and is labelled as the snapshot size exactly as prescribed. ✓
- **Artifacts (report 8-10):** cites sections "A / B / B2 / B3 / C / E / F" and says "no line count is quoted on purpose — it drifts on every re-run by design" — no line count anywhere; the section list matches the artifact's headers exactly (2/31/78/90/118/174/217). ✓
- **Headline table (report 24):** round 6 all rows 1,058 / 78.8% / 18.0% / 128K — matches main-loop rows 1058 (124), main reporting 78.8% with resets 190 (18.0%) (130), and avg prompt 135,133,018 / 1,058 = 127,690 ≈ 128K. ✓
- **Traced window (report 25):** 25 rows / 63.5% / 120K — matches section F: 25 rows (222-246), window total "2,995,687 prompt tokens, 1,902,848 cached → hit 63.5%" (247); 2,995,687 / 25 = 119,827 ≈ 120K. ✓
- **Per-model (report 34-36):** all six match the artifact (135-140) exactly: deepseek-v4-flash 83.7% n=454 · :0731 74.7% n=270 · v4.1-flash 72.2% n=166 · glm-5.3-flash 78.1% n=137 · glm-5.3 79.7% n=19 · kimi-k3 42.4% n=12. ✓
- **Caveat (report 117-118):** 1,064 rows / 22 sessions — matches the artifact. ✓
- **Snapshot paragraph (report 27-32):** attributes the table to the producing run "(1,064 rows / 22 sessions then)" (matches 120/123) and names both earlier drafts: "checked against 972 and 1,043 rows" — the claimed history, coherently told. ✓

**Remaining numbers swept — no mismatches.** Section B/B2/B3-derived numbers in the report (5 of 5 MID pairs at k ≈ 271, 275, 276, 280, 286 ~20 from the end; byte offsets 504-517; length pairs 4,336→717 / 2,831→546 / 2,697→589 / 8,745→711 / 1,193→544; B2 YES on 3→4 / 4→5 re-billing 36-41K — post-break 40,729 / 36,161; B3 intact 68-71, short 60-63, truncatable 7-8) all match the artifact's unchanged B/B2/B3 sections (byte-identical to what round 2 verified — this commit's artifact diff touches only C and the bucket rows). The 320-360K bin 96.9% and max prompt 322,619 (report 98) match the histogram (160) and per-model maxP (135). The head-rewrite numbers (26,993 → 25,831 chars, tools 30 → 26, cached 6,016; 7 window rows at ~5% with TTFT 15.7 s / 17.9 s) match section A and F (records 5/6; rows 0,1,6,7,13,14,15; ttft 15,772 / 17,874 ms). Two nuances checked and NOT findings (see Notes): the E section's static "7 of 24 window rows" text vs F's 25 listed rows, and the report's pre-fix code line citations.
---

## Finding 3 — FIXED: extractor ↔ artifact now consistent and reproducible

- **(a) Docstring (extract.py 13-27):** the Sections list now covers exactly A (14), B (15-17), B2 (18-20), B3 (21-22), C (23-25), E (26), F (27) — all seven, in emission order, none missing, none invented. ✓
- **(b) No section-"D" reference:** grep for `\bD\b` over the committed script returns exactly one match — line 607, `w("D. INSTRUMENT: ...")`, which is the E section's internal verdict-item lettering (A/B/C/D inside the hand-written mutation-path verdict list, mirrored at artifact 177/195/208/212). That is a verdict label within section E — a namespace the docstring already describes as "hand-written from the code read" — not a top-level artifact section, and the E section is byte-identical to the artifact round 1 verified. The stale reference round 2 targeted (the up-front comment's "sections A/B2/C/D") is gone: lines 109-110 now read "sections B2/C/E/F all need it (B2 runs before C and cannot re-query)". ✓
- **(c) Reproducibility (label/format equality):** the script's rows-line format string (436) is `rows: {len(rows)}  (loaded up-front — sections B2/C/E/F share the same set)`; the committed artifact (120) prints `rows: 1064  (loaded up-front — sections B2/C/E/F share the same set)` — same label, same wording, same two-space separator. Re-running the committed script would change only the number (the DB accumulates — expected and covered by the report's snapshot paragraph), not the line's text. Section headers in the artifact are exactly A, B, B2, B3, C, E, F with single occurrences each (no duplicated, no `(pending)`, no placeholder banners anywhere in the 247 lines). ✓
- **(d) Internal arithmetic spot-checks — all consistent with the script that produced it:**
  - **B3:** intact = short + truncatable holds for every non-empty record (rec 1: 60+8=68; rec 6: 62+7=69; rec 8: 62+8=70; rec 9: 63+7=70; rec 10: 63+8=71 — all nine check), and toolmsgs = marked + intact holds too (rec 1: 93+68=161 … rec 10: 99+71=170), matching the script's `intact = short + truncatable` (420) and the three-way `marked/short/truncatable` partition (412-419). ✓
  - **C:** hit% 106,513,220 / 135,133,018 = 78.82% → 78.8% ✓; resets 190/1,058 = 17.96% → 18.0% ✓; per-model n sums to 1,058 (454+270+166+137+19+12) ✓; histogram bins sum to 1,058 (107+275+159+152+138+144+69+9+5) ✓.
  - **Buckets:** 1,020+22+16 = 1,058 ✓; miss tokens 26,945,791+269,451+1,404,556 = 28,619,798 ✓; shares recompute correctly (94.2% / 0.9% / 4.9%; 96.4% / 2.1% / 1.5%) ✓; reconciliation line "buckets 1058 == main-loop rows 1058 -> OK" ✓.
  - **F:** summing the 25 listed rows gives exactly 2,995,687 prompt and 1,902,848 cached → 63.5%, matching the printed window total (247). ✓

  The script's only write is its own generated `OUTPATH` (`with open(OUTPATH, "w", ...)` at 643-644, `OUTPATH = ".coding/analysis/cache-hit-6-aggregates.txt"` at 38) — the sanctioned generated-artifact pattern accepted in rounds 1-2; no tracked file is shell-mutated. ✓
---

## Round-3 duties — all checked

- **Commit completeness:** exactly the five claimed files in the stat; no file that does not belong; 138 insertions / 31 deletions as stated. ✓
- **Comment-only src delta:** verified from the diff (only `///` lines 926-928 changed) and by reading the surrounding committed code — the function body, helper, and regression test are exactly the round-1/round-2-verified state. ✓
- **Tree clean at HEAD:** `git diff HEAD` and `git status --short` both empty (no untracked either); HEAD `0aed033`, parent `f1a5809`. ✓
- **`cargo test` 2329 passed / 0 failed / 5 ignored:** I cannot execute it (reviewer toolset is read-only, no shell); verified consistent by construction — the only Rust change is three doc-comment lines. Doc comments are stripped at parse and cannot change semantics; the doc block remains contiguous and syntactically valid before `pub fn compact_old_tool_results` (949-950); no doctest is introduced; `#![deny(warnings)]` was already green on the identical code semantics in round 1's verified run. A comment/doc-only delta cannot flip a test outcome. ✓
- **No `#[allow(...)]`:** zero matches in `src/agent/context.rs` (the commit's only Rust file; the repo-wide grep hits are vendor/tao and unrelated doc comments elsewhere). ✓
- **Multi-platform neutrality:** doc-comment prose plus markdown; the Python changes are stdlib-only with relative forward-slash paths and read-only sqlite URIs (`mode=ro`). No `cfg(windows)`, no Windows paths, no shell syntax. ✓
- **File-tools-first:** no shell mutation anywhere in the changeset; the script's single write is its own generated OUTPATH (643-644). ✓
- **Documentation sync:** nothing owed — this commit IS the documentation sync round 2 demanded (the report and script docstrings are the docs for an internal analysis artifact; no README/PLAN.md/user-facing surface touched). The commit message accurately describes all four changes. ✓

## Notes (checked, not findings)

1. **E-section internal "D" lettering:** the artifact's E section carries verdict items A/B/C/D ("D. INSTRUMENT" at 212, script 607). These are verdict labels inside a section the docstring describes as hand-written; they are not top-level section references and E is unchanged since round 1. The Finding-3(b) grep criterion is satisfied — no script comment/string names a top-level section "D".
2. **"7 of 24 window rows" (E, static text) vs F's 25 listed rows:** the report (85-86) quotes the artifact's E text exactly; F's 25 rows span the padded window disclosed in the F header. This coexistence predates the commit, E's text is script-static (so the artifact remains byte-reproducible), and round 2 already accepted the window shorthand as matching LOW-2's success criterion. Not re-flagged.
3. **Pre-fix code line citations in the report** (`context.rs:829-851`, `950-963`, gate "~969") and in the E section describe the measured pre-fix layout — historical evidence, blessed as accurate-as-written by round 2; the post-fix gate sits at 992. Carried over, not re-flagged.

**Conclusion.** All three round-2 findings are fixed exactly as described, completely: the rule-1 sentence now states the truncatable gate (consistent with the hysteresis section, the code, and the B3 data); every report number matches the committed artifact; the script docstring/comment/artifact are mutually consistent and label-reproducible. Nothing else moved: comment-only src delta, clean tree, five claimed files, no `#[allow]`, no platform-specific code, no shell mutation. Verdict: PASS — the plan's closing documentation/artifact consistency loop is closed.