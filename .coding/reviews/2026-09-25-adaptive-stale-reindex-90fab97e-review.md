## Verdict: FINDINGS (0 high, 1 low)

The code change is correct and complete — all six verification points check out by trace; the single low finding is commit hygiene for the `.coding/` artifacts (a supersede pair must land whole), not a code defect.

### Scope reviewed

The full uncommitted working tree on `wt/mnemo` (tip 6ad8fad): 8 modified files (`src/codegraph/mod.rs`, `src/tool/agent/search.rs`, `src/tool/agent/codegraph.rs`, `src/tool/agent/search_read.rs`, `PLAN.md`, `docs/FEATURES.md`, `.coding/backlog.jsonl`, `.coding/knowledge/decision/2027-01-05-content-index-staleness-repairs-inline-8-files-i.md`) plus 4 untracked artifacts (decision 2027-01-11, BUG 2027-01-11, plans 90fab97e + e36d0a54).

### Verification point 1 — the refreshed count can never over-report

Traced `try_index` (search.rs:973-1009) across both attempts:

- `reindexed` surfaces only on the `Hits` arm, and `Hits` is reachable only when the attempt-1 re-query returns `stale_paths.is_empty()` ��� every file on the re-served page is fresh. `reindexed = refreshed` (the ACTUAL count) is an improvement over the old `reindexed = page.stale_paths.len()` assumption, which could over-report when an individual upsert/prune failed inside the pass while `n > 0` still held.
- Mid-set budget overrun: `refreshed = 5` of 9 → `continue` → the attempt-1 re-query still flags the 4 un-refreshed hit files (`fts_page` recomputes mtime mismatches from the new page, search.rs:927-943) → `Stale { files: 4 }` — the `reindexed` value is discarded and the note is the honest "content index stale for 4 file(s)" walk note. Nothing stale is served.
- Busy pass: `Ok(0)` → `unwrap_or(0)` → `refreshed == 0` → `Stale` (identical to the old `is_ok_and(|n| n > 0)` gate); the pre-existing flag is never touched.
- Only theoretical residual (pre-existing, not introduced): if a still-stale file falls outside the 500-hit fetch on the re-query after a ranking shift, attempt 1 serves `Hits(reindexed=5)` — but the served page is entirely fresh and "reindexed 5" is literally true; page-scoped freshness was always the F10 contract.

### Verification point 2 — flag-claim semantics unchanged

`reindex_stale_files` (codegraph/mod.rs:587-689): the `compare_exchange` claim is untouched — a busy pass returns `Ok(0)` without touching the flag (:588-596). `IndexFlagGuard` (:102-108) is created immediately after a successful claim (:597) and its `Drop` releases on every exit: the budget `break` falls through to the `rebuild_edges()` + `Ok(refreshed)` tail with the guard alive until return, the `?` on `rebuild_edges` drops it on the error path, and a panic unwind runs `Drop` (the budget additions — `Instant::now`/`elapsed` — cannot panic). Pinned by `reindex_stale_files_busy_returns_zero_and_leaves_the_flag` (unit) and `stale_while_an_index_pass_runs_walks` (tool level, asserts `graph.is_indexing()` survives).

### Verification point 3 — containment branch unaffected

The budget gate (:611-619) sits at the TOP of the loop body — before the vanish-prune read, before `canonicalize`, and before the `canon.starts_with(&root_canon)` containment check (:641-648), which is byte-identical to before. An early break leaves an out-of-root key stale exactly the way the skip branch does (caller walks). `reindex_stale_files_skips_out_of_root_keys` still passes a generous budget and pins the skip.
### Verification point 4 — exactly ONE re-query

Control flow is diff-equivalent to the old structure: `continue` fires only when `refreshed > 0` (old: `is_ok_and(|n| n > 0)`); `refreshed == 0`, above-cap, and attempt-1 all fall through to the shared `return Some(FtsOutcome::Stale { … })`. Max 2 `fts_page` calls, max 1 reindex per `try_index` call; attempt 1 can never re-repair (the `attempt == 0` guard). The new inner `if refreshed > 0 { …; continue; }` adds no path to a second repair.

### Verification point 5 — bounded worst case

Cap 32 × the 500 ms between-files gate bounds the pass. The gate placement (checked BEFORE each file) is documented in both the `STALE_REINDEX_BUDGET` const doc (mod.rs:201-208) and the loop comment (:612-616): a spent budget stops the pass without starting another file's parse, so the overshoot is bounded by one file's parse — the same single-file exposure the old 8-file pass already had, now amortized under an `Instant` budget. `Instant` (monotonic, immune to wall-clock jumps) is the right clock, and it starts AFTER the flag claim so the claim is never charged to the budget. The watcher's debounce wait on the held flag is bounded by the same budget. `Duration::ZERO` is a documented no-op, so the gate is deterministic at the boundary.

### Verification point 6 — the new tests genuinely fail on the old code

- `nine_stale_files_reindex_inline` (search.rs:3294): on the old code 9 > 8 → attempt 0 never repairs → `Stale` → walk with "content index stale for 9 file(s)" — the `contains("reindexed 9 stale file(s)")`, `contains("engine: index")`, and `!contains("engine: walk")` assertions all fail. It asserts the fresh-index path itself ("needle v2" served from the index), so it cannot pass vacuously. The +5 s mtime bump rules out mtime-granularity false negatives.
- `reindex_stale_files_respects_the_budget` (mod.rs:1524): `Duration::ZERO` → the gate trips before file 1 → `Ok(0)` with both markers unsearchable (pins "budget spent → nothing refreshed → caller walks"); a generous budget refreshes both, `Ok(2)`. Against the old one-arg signature it does not compile — trivially fails.
- `stale_above_the_reindex_cap_walks` re-pinned correctly: `let cap = STALE_REINDEX_CAP` → 33 files → 33 > 32 → walk, note "index stale for 33 file(s)" via `format!(…, cap + 1)`.
- The other three F10 tests are 1-file / busy-pass paths, behaviorally unchanged; `search_read.rs`'s only staleness test is its 1-file variant. The symbol-index cap in codegraph.rs (:239, still 8), its `stale.len() <= STALE_REINDEX_CAP` gate (:340), and its 9-file-boundary tests are deliberately untouched per plan 55163f1e.

Live corroboration: my own `search` calls during this review (e.g. "at most 8" over `**/*.md`) returned "note: reindexed 1 stale file(s) — serving fresh index results" with index-engine results — the adaptive path repaired this plan's own just-edited `.coding/` artifacts mid-session, the exact watcher-lag scenario it exists for.

### Standing checks

- **Documentation sync — PASS.** docs/FEATURES.md (the FTS bullet) and PLAN.md:1369-1377 both describe the new bound accurately (32-file ceiling, ~500 ms budget, budget-spent → walk). README.md carries no F10 staleness wording (only an unrelated "stale context" line at :28) — nothing to update there. All touched module docs match the code: the `STALE_REINDEX_CAP` doc (search.rs:341-356), `FtsOutcome::Stale` (:844-851), `try_index` (:961-972), the call-site comments in search.rs (:1379-1384) and search_read.rs (:268-273), and the codegraph.rs sweep comment (:321-329). The codegraph.rs module doc's "up to 8 stale files inline" (:36) remains correct — it describes the SYMBOL index, whose cap is deliberately still 8. The historical `.coding/reviews/2027-01-04-…` file mentioning the old "at most 8" wording is immutable history, correctly left alone. The only "8 stale" remnant in the tree is codegraph.rs:36 (correct as above); no other stale-boundary wording remains.
- **Multi-platform neutrality — PASS.** Pure `std` throughout: `Instant::now`/`elapsed` (monotonic), `Duration::from_millis`/`ZERO`, `std::fs::File::options().set_modified` (std — SetFileTime on Windows, utimensat on Unix). No `cfg(windows)`, no platform paths, no shell syntax anywhere in the change. Builds and behaves identically on macOS and Windows.
- **File-tools-first — PASS.** No shell-based file mutation in the diff; everything is source/doc edits.

### `.coding/` artifacts reviewed

- The 2027-01-05 decision file correctly gains `status = "superseded"`; the new 2027-01-11 decision record documents the two-tier bound, the deliberate non-choices (no background pass on overrun; symbol-index cap untouched), and the open root cause — coherent with the code.
- The BUG record (root causes: the 800 ms watcher debounce + `.coding/` indexed-but-never-watched) matches the source it cites and correctly frames this change as a mitigation with the watcher fix queued as the next plan (user decision 2027-01-25).
- `.coding/backlog.jsonl` changes are app-managed state (the 9201704f "interrupted… returned to queue" note and the 652ae094 done-item sweep), not hand edits — fine to ride the commit. Bookkeeping note: item 9201704f still reads status "pending"; the parent's finish flow should mark it done (backlog_status is parent-side — not actionable from this review).
- `.coding/plans/90fab97e.md` is the finished plan (7/7 checked); `.coding/plans/e36d0a54.md` is the completed branch-prep research plan from the prior session — both belong in the commit per the `.coding/` side-car policy.

### Test execution note

I could not execute `cargo test` myself — my spawn-time allow-list carries no shell tool. Execution evidence is the parent's reported green run (2714 passed / 0 failed / 5 ignored, plus 19 integration and 1 doc-test, warning-free under `deny(warnings)`), corroborated by the static trace of every new/changed test above. Since the single finding below requires no code change, the closing-sequence re-run should confirm the same green.

### Finding

**LOW-1 — commit the untracked `.coding/` records together with the tracked supersede, or the committed knowledge store goes inconsistent.** The modified `.coding/knowledge/decision/2027-01-05-content-index-staleness-repairs-inline-8-files-i.md` adds `status = "superseded"` pointing at its superseder, `.coding/knowledge/decision/2027-01-11-content-index-staleness-repairs-inline-under-an.md`, which is UNTRACKED — as are the BUG record and both plan files. Committing only the tracked modification would land a superseded record whose superseder is absent from git — the exact dangling-supersede failure the project itself documented (plan e36d0a54: "committing only one half of a supersede pair corrupts the knowledge store, so both halves must land"). Fix at the commit step: stage the full `.coding/` set with this change — the modified decision file, the new decision + BUG records, `backlog.jsonl`, and both plan files — alongside `src/`, the docs, and this review report.