## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of the R19 fix commit f538df7 (HEAD on wt/agenticcoding, clean tree confirmed) for backlog fd8f67c0 / plan f8858f62: both round-1 findings are correctly fixed and verified in the committed file state (not just the diff); the regenerated aggregates reconcile exactly across all five cross-computation paths; the script stays read-only over memory.db; no Rust files touched. One new LOW: the exec-summary sync missed one number — the §3 bullet's "83.1% next day at nonN=0" is from the pre-regeneration window; the final committed A2 shows 09-10 at 82.8%.


### Round-1 fix verification

**LOW 1 (A4 header wording) — fixed correctly.** `emit_post_r12` (script line 161) now takes `note='real provider-reported cached tokens'` as a default parameter; the non-reporting call (line 184) passes `note='cached_tokens field absent — 0 is not a miss'` and drops the now-redundant "(cached always 0)" from the label. Committed output: the non-reporting A4 header (aggregates line 139, exactly where the brief predicted) reads "--- A4. Post-R12 subset, non-reporting tier (cached_tokens field absent — 0 is not a miss) ---"; the all-rows (line 102) and reporting (line 123) variants keep the accurate default suffix. The contradictory double-parenthetical is fully gone; the em-dash renders correctly (script writes UTF-8, line 298). Repo-wide sweep for the suffix wording: the only other occurrences are the round-5 review/plan (historical, describe the R12 fact for reporting rows — accurate in frame) and the exec summary's "Data basis" line (round-5 record, pre-existing round-1 observation) — nothing quotes the old contradictory header except the round-1 review itself, which is the historical defect record and correct to keep.

**LOW 2 (A2 rep-column dashes) — fixed correctly, latent as predicted.** The `rep_cols` conditional (script lines 128–129) prints two 8-wide dashes when a day has no reporting rows, matching A3's `agg()` empty-set dash convention; the non-empty branch is byte-identical to the old inline format (same `:>8` widths), so no committed number moved. Confirmed latent: the current window's minimum daily repN is 86 (09-01); every A2 row has repN>0 and no dashes appear. The only A2 row that changed vs d4a5ea6 (09-10: 961→979 rows) is regeneration data drift, not the fix.

**Exec-summary sync — headline numbers exact; one sub-detail missed (the finding).** §3 bullet (line 101): 79.8% hit / 18.1% resets ✓ (A0 line 19 and A4 line 124: n=14751, 79.8%, 2664 (18.1%)); 29,759 rows through 09-10 ✓ (aggregates line 4); fp8-01 57.8% ✓ (A1 line 36); 09-09 dip 41.8% combined / 51.9% reporting-tier ✓ (A2 line 69). But "83.1% next day at nonN=0" is stale — see Findings.

### Regression sweep — no regressions found

- **Read-only preserved:** line 35 `sqlite3.connect('file:.coding/memory.db?mode=ro', uri=True)` unchanged; the script's only write is the aggregates .txt (lines 298–299); traces.jsonl / provider-errors.jsonl opened read-only.
- **A0 reconciliation exact:** all-time rows 27,544+2,215=29,759 (of 29,759), resets 3,435+2,215=5,650 (of 5,650); post-R12 rows 14,751+2,215=16,966 (of 16,966), resets 2,664+2,215=4,879 (of 4,879) — the brief's two identities confirmed, plus the all-time pair.
- **Five independent cross-computation paths recomputed, all exact:** (1) A0 lines as above; (2) A1 per-model n sums — 13 R-tier models → 27,544, 5 N-tier (761+412+1,013+16+13) → 2,215; (3) A2 daily sums — Σn=29,759, ΣrepN=27,544, ΣnonN=2,215, Σresets=5,650, post-R12 Σn=16,966 / ΣrepN=14,751 / Σresets=4,879; (4) A3 bucket partitions — all-rows n = reporting + non-reporting in all 7 buckets, both windows (e.g. 0K-50K 5,874=5,175+699; post-R12 3,746=3,047+699); (5) A4 — >340K rows 49=34+15, per-model sums 16,966 / 14,751 / 2,215. The regenerated .txt is a genuine product of the committed script, not a hand-edited artifact.
- **Non-reporting exactly 0.0%/100.0%** in A0, A1 (all 5 N rows), A3 (all non-reporting buckets), and A4 — by construction, as before.
- **B-section shrink (9→2 traces) is the known non-issue** (traces.jsonl rollover, per the brief); C grew 477→486 errors / C4 261→270 — normal drift from the later run; C formats unchanged.
- **No Rust/frontend files touched** — f538df7's diff is .coding/-only (5 files), consistent with the reported green cargo test (2221+16, 0 failed); not re-runnable by this read-only reviewer.
- **Plan bookkeeping:** step 5 of plan f8858f62 checked off — consistent with the fix commit completing the plan.

### Findings

**LOW (new, introduced by this commit's partial sync) — exec summary §3 bullet: "83.1% next day at nonN=0" is stale.** The bullet now anchors itself to "the regenerated window, 29,759 rows through 09-10", but 83.1% is 09-10's combined hit from the pre-regeneration output (d4a5ea6: 961 rows); the final committed A2 shows 09-10 at 82.8% (979 rows, nonN=0 — aggregates line 70). Every other number in the bullet matches the final aggregates; this one was missed. Interpretation is unaffected (recovery to ~83% either way), but the committed doc and the committed data disagree. Fix: file_edit .coding/analysis/2027-01-09-review-round-executive-summary.md line 101, "83.1% next day" → "82.8% next day" (or "~83%"). No regeneration needed.

### Observations (not findings)

- The exec summary's "Data basis" line (line 15) still describes the round-5 window (28,506 rows / 15,713 post-R12 / 431 errors) — pre-existing round-1 observation ("both individually accurate": round record vs shipped outcome); f538df7 didn't touch it.
- The A2 legend still says "nonN = non-reporting rows (cached always 0)" — factually true (the value is 0) and not contradictory; the contradiction fixed in LOW 1 was the "provider-reported" claim. No action needed.
- The DECISION record and round-5 review/plan cite their own commit-time windows — historical records, accurate in frame, untouched by f538df7.

### Verification note

Read-only reviewer — no shell; could not re-run the extraction or cargo test. Verified by inspecting the committed file state at f538df7 (clean tree confirmed via empty `git diff HEAD`) against the fix diff, the d4a5ea6 original, and the round-1 report, with all aggregate math recomputed by hand.