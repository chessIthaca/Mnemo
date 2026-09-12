## Verdict: FINDINGS (0 high, 2 low)

R19 (backlog fd8f67c0, plan f8858f62) at d4a5ea6 (HEAD, wt/agenticcoding): the reporting-tier split is correctly implemented, the regenerated aggregates are internally consistent across every reconciliation path I could cross-compute, the script stays read-only over memory.db, the exec-summary amendment is accurate against the data, and the skipped reports_cached_tokens schema flag is a sound, well-documented decision. Both findings are cosmetic wording issues in the generated artifact — neither affects any number.


### What was reviewed

- `git show d4a5ea6` (full diff): `.coding/analysis/cache-hit-5-extract.py` (the R19 substance — A0 tier split, A1 tier column, A2 rep/non columns, A3/A4 per-tier helpers, A5 reporting-only table), the regenerated `.coding/analysis/cache-hit-5-aggregates.txt`, the §3 amendment in `.coding/analysis/2027-01-09-review-round-executive-summary.md`, the DECISION record, plan f8858f62, backlog.jsonl bookkeeping, and the two status-bar sidecar artifacts (glance only, per the brief).
- Current file state of the script (298 lines) and the full aggregates output (431 lines), cross-computed against each other.
- Repo-wide reference check for stale pointers to the changed artifacts (nothing parses the .txt layout — all references are prose).
- No Rust/frontend files touched (the diff is .coding/-only); cargo test green (2221+16) as reported — not re-runnable by this read-only reviewer.

### Verified correct

**1. Tier classification semantics (the core of R19).** `REPORTING = {m for m in models if any(r[5] > 0 for r in rows if r[0] == m)}` — model-level, window-wide. Right semantics: a reporting model's genuinely-cold rows (hit 0) stay in the reporting tier and count as real resets (for a model that reports, a 0 IS a miss), while a never-reporting model's rows are excluded from the baseline. `tier_rows` partitions on `(r[0] in REPORTING) == reporting`; since every row's model is in `models`, the partition is complete — which is why the reconciliation lines are guaranteed to add up (they would catch a `tier_rows` bug if one existed). cached_tokens is INTEGER NOT NULL (src/memory/schema.rs:106, per the plan context), so the `> 0` test is NULL-safe.

**2. Reconciliation math — cross-computed, all exact.** Five independent paths recomputed; every one agrees:
- A0: all-time rows 27,526+2,215=29,741 (of 29,741); resets 3,430+2,215=5,645 (of 5,645). Post-R12 rows 14,733+2,215=16,948 (of 16,948); resets 2,659+2,215=4,874 (of 4,874).
- A1 per-model n sums to the tier totals: the 13 R-tier models sum to 27,526; the 5 N-tier models (761+412+1,013+16+13) to 2,215.
- A2 daily: Σn=29,741, ΣrepN=27,526, ΣnonN=2,215, Σresets=5,645; post-R12 days (08-31 onward) Σn=16,948, ΣrepN=14,733, Σresets=4,874 — each matches A0/A4 independently.
- A3 buckets: all-rows n = reporting n + non-reporting n in all 7 buckets, both all-time and post-R12 (e.g. 0K-50K: 5,872=5,173+699; post-R12 3,744=3,045+699); the non-reporting bucket n's sum to 2,215.
- A4: prompt>340K rows 49 = 34 (reporting) + 15 (non-reporting); hit distributions sum to 100.0% in all three variants.

**3. Acceptance criteria met.** Post-R12 reporting tier 79.9% hit / 18.0% resets vs the item's "~81% / ~17%" ballpark (the plan explicitly predicted numeric drift — the window grew from 28,506 to 29,741 rows through 09-10); non-reporting exactly 0.0% / 100.0% — provable from the heuristic's definition (any model with a cached_tokens>0 row would be reporting, so every non-reporting row has cached=0 → hit 0 → reset), i.e. "by construction" is precisely right.

**4. Static-list cross-check works as designed.** The cross-check line flags 4 stale entries (deepseek-v4-flash, deepseek-v4-flash-gcp, glm-5.2-maas-gcp, glm-5.3-flash-gcp-h200-fp8-01 — heuristic=reporting, round-5 list=non-reporting) and "unlisted non-reporters: none". The first three were already reporting in the round-5 window (18.6% / 49.2% / 17.5% hit — the round-5 review's list was wrong about them even then); fp8-01 is the self-adaptation demo: 245 rows / 0.0% in the round-5 extraction, now 864 rows / 57.8% and auto-moved to R without any list edit.

**5. Read-only over memory.db preserved.** Line 35 `sqlite3.connect('file:.coding/memory.db?mode=ro', uri=True)` unchanged; the script's only write is the aggregates .txt (line 296); traces.jsonl and provider-errors.jsonl are opened read-only. PRAGMA busy_timeout is a connection setting, not a write.

**6. No division-by-zero / crash paths.** `pct()` guards b=0; `hit_of()` guards prompt_tokens=0; A5's ranking guards `sum(r[2])>0`; every `statistics.mean` is guarded (`if prompts else 0`, `if rs else 0`) or on provably non-empty lists (day groups); A2's `max()` is over non-empty day lists. A5's sort key (hr, sid, rs) never reaches the list element (session ids are unique dict keys). A1/A2 header-vs-data column widths match field-by-field. The UTF-8 BOM on line 1 is preserved (per the plan's instruction).

**7. Documentation sync.** The §3 amendment's numbers all check out against the regenerated output: 79.9%/18.0%, 0%/100% by construction, fp8-01 57.8% moved tiers, 09-09 dip 41.8% combined / 51.9% reporting-tier / 83.1% next day at nonN=0 (A2 rows 09-09/09-10 exactly). The script's module header remains accurate; the DECISION record is compact and pointer-first (points at A0 + the exec summary). Nothing parses the .txt layout — every reference (provider-comms/perf-comm reviews, plan 4422c64c, backlog items f3982dae/bd9df146) is a prose section pointer, and the C-section formats are unchanged so those pointers still resolve. The round-5 review file itself is untouched (reviewer-protected), as required.

**8. The skipped schema flag (optional step 2) — sound.** Four reasons, each verified:
- It was explicitly optional in the backlog item, and the item's acceptance criteria (split visible in the next extraction; script read-only) are met without it.
- The self-adaptation argument is demonstrated in-window: fp8-01 moved tiers automatically when the provider started reporting. A flag backfilled false would have gone stale within days and silently re-contaminated the reporting baseline until manually re-backfilled — and detecting the need to re-backfill requires exactly the window heuristic the flag was meant to replace. Schema surface + migration for negative accuracy gain.
- The write-site argument is confirmed by the queued R21 item (bd9df146) itself, which plans `cached = NULL (not 0 — distinguishable from a real miss)` for error rows and reworks the recording path — building the flag now would land on a path queued for rework.
- The heuristic's own failure mode (a reporting model with a genuinely all-cold window misclassified as non-reporting) is bounded and conservative: it understates the baseline, and the cross-check line surfaces "unlisted non-reporters" as a tripwire (currently none).

**9. Constitution checks.** Multi-platform neutrality: pure stdlib Python, relative forward-slash paths, output written with `newline='\n'` (deterministic LF on all platforms — good for a committed artifact); no OS-specific code. File-tools-first: the .py and exec-summary edits went through the file tools (per plan steps), the .txt is the script's designed output (regeneration, not shell surgery), knowledge files via the knowledge tooling, backlog.jsonl app-managed — no shell-based mutation anywhere in the change. No Rust files touched; the reported green cargo test is consistent with the diff.


### Findings

**LOW 1 — A4 non-reporting header carries a contradictory suffix.** `emit_post_r12` hardcodes "(real provider-reported cached tokens)" into every variant's header, so the non-reporting table reads "--- A4. Post-R12 subset, non-reporting tier (cached always 0) (real provider-reported cached tokens) ---". The suffix is precisely wrong for that tier — the zeros are field-absent (the exact distinction R19 exists to make), not provider-asserted. The suffix predates R19 (it distinguished provider-reported from client-estimated tokens) and was accurate for the single all-rows section; the per-tier emission makes it contradictory for the non-reporting variant (and half-true for all-rows). Fix: parameterize the suffix or drop it for the non-reporting call — one line, then re-run the script. Wording only; no number is affected.

**LOW 2 — A2 rep columns would print '0.0%' on a repN=0 day (latent).** `pct()` returns '0.0%' for a zero denominator, so a day with only non-reporting traffic would print repHit%=0.0% / repRes%=0.0% — reading as "0% hit" rather than "no reporting rows". A3's `agg()` uses the '-' convention for empty sets; A2's rep columns don't. Latent only (the current window's minimum daily repN is 86, on 09-01), and no number in the committed output is affected; a one-line conditional (dashes when `drep` is empty) would align A2 with A3.

### Observations (not findings)

- The exec summary's "Data basis" line still describes the round-5 window (28,506 rows, 15,713 post-R12, 431 errors) while the amended §3 bullet cites the regenerated window (29,741 rows through 09-10). Both are individually accurate (round record vs "Shipped" outcome), but a reader comparing them may wonder which window 79.9%/18.0% come from. Optional: a clause like "(regenerated window, 29,741 rows through 09-10)".
- The DECISION record's created date (2027-01-07) predates the shipped date in its body (2027-01-10) — same convention as the sibling status-bar spec committed alongside; no action.
- Backlog fd8f67c0 is in_flight at this commit (with a 429 dispatch-error note) — the normal sequence; done-stamping happens at finish after this review passes.
- Sidecar artifacts (glance): the status-bar round-2 review is reviewer-authored with a proper PASS verdict header; the spec/plan/backlog bookkeeping is consistent with the parallel item's lifecycle; nothing anomalous.
- Sections B/C regenerated with new data (traces 40–48, errors through 09-10, C4 rows 261) — expected whole-file regeneration; C-section formats unchanged.

### Verification note

Read-only reviewer — no shell; could not re-run the extraction or cargo test. The aggregates were verified by inspection and cross-computation: five independent reconciliation paths (A0 lines, A1 per-model sums, A2 daily sums, A3 bucket partitions, A4 >340K split) agree exactly, which is strong evidence the .txt is a genuine product of the committed script rather than a hand-edited artifact. The reported green cargo test (2221+16, 0 failed) is consistent with the diff touching no Rust files.
