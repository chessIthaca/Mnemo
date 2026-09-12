## Verdict: PASS

Round-3 verification, scoped to round 2's single residual (L1) of plan 44387fcd (`.coding/reviews/2026-09-17-findings-round-f1-f13-verify-review.md`): the discriminating regression test for finding 3's widened whole-page freshness scan is present, correct, and genuinely fails on the pre-fix `hits`-scoped code. Commit 8e90dc0 (HEAD, tree clean) touches only the test file and the round-2 report. No new issues. The 1684-total arithmetic is consistent (1682 → +1 → 1683 → +1 → 1684).

## 1. Test location and assertions

`stale_file_beyond_the_display_cap_still_surfaces` (`src/tool/agent/search.rs:2017-2068`) sits immediately after `stale_index_serves_fresh_walk_results` (:1985-2015), exactly as the L1 fix description states. It asserts all three discriminating properties: `r.output.contains("index stale for")` (:2057-2061), `r.output.contains("engine: walk")` (:2062), `r.output.contains("tail-refreshed")` (:2063-2067), plus the `r.success` precondition (:2056). Fixture matches the commit message: 100 strong files `s000.md`..`s099.md` ("needle needle needle\n") + weak `a-stale-tail.md` (40× "filler " + one "needle" + tail marker), indexed via `make_indexed_tool`, then only the weak file rewritten ("tail-v1" → "tail-refreshed") with mtime bumped 5s into the future.

## 2. The test genuinely discriminates

Mechanics confirmed against the code:

- **Cap/fetch sizes.** `MAX_MATCHES = 100` (`search.rs:324`); the raw index fetch is `MAX_MATCHES * 5 = 500` rows (`search.rs:586`) — 101 matching rows fit comfortably, so the weak file is always on the fetched page.
- **Ranking.** `Store::search_content` orders `ORDER BY rank` ascending (`src/codegraph/store.rs:543-545`) and `ContentHit.rank` is documented "FTS5 bm25 rank (lower = better)" (`store.rs:77`). With FTS5 default bm25 (k1=1.2, b=0.75): the strong rows (tf=3, doclen=3, avgdl≈3.4) score ≈ 3/(3+1.2·(0.25+0.75·3/3.4)) ≈ 0.73; the weak row (tf=1, doclen≈42) scores ≈ 1/(1+1.2·(0.25+0.75·42/3.4)) ≈ 0.08 — the strong rows rank ~9× better, so all 100 fill the display cap and the weak hit is 101st. The idf factor is identical for every row (df=N=101), so it cannot reorder; even at b=0 the saturated tf (3 vs 1) still puts the strong rows first. The weak file is therefore fetched and counted into `total`/`files` but never pushed to `hits` (cap at `search.rs:598-600`).
- **Fixed code path.** `try_index` inserts into the `files` set for every glob-passing hit BEFORE the display cap (`search.rs:597`), and the freshness loop iterates `&files` (:621-635). The weak file's on-disk mtime (`now + 5s`, integer seconds) is strictly greater than the stored as-of-index mtime captured by `index()` (`mtime_of`, `src/codegraph/mod.rs:384-391`), so `on_disk != stored_mtime` → `stale_files = 1` → `FtsOutcome::Stale` (:636-637) → the caller emits "note: content index stale for 1 file(s) — serving tree-walk results" (`search.rs:893-896`) and falls through to the walk. All three assertions pass.
- **Pre-fix (hits-scoped) code.** `files` would contain only the 100 displayed strong files (never rewritten, mtimes match) → `FtsOutcome::Hits` → the output is "301 matches in 100 files (engine: index)" (:878-883) with only strong-file lines — no "index stale for", no "engine: walk", no "tail-refreshed". All three assertions fail, so the test is a true regression pin, not a redundant F10 duplicate (which round 2 correctly noted `stale_index_serves_fresh_walk_results` is).

The mtime bump is deterministic: stored mtime ≤ index-time seconds; on-disk = (execute-time + 5s) truncated to seconds, strictly greater regardless of filesystem mtime granularity.

## 3. Walk-side assertions hold

- **Ordering.** `walk_searchable` → `descend` sorts each directory's entries by file name (`search.rs:298-299`); all 101 files live in the sandbox root, and `"a-stale-tail.md" < "s000.md"` under both UTF-8 (macOS) and UTF-16 (Windows) byte ordering, so the weak file is visited first.
- **Cap.** Results are capped at `MAX_MATCHES` lines (:931, :948); the weak file's single matching line becomes result #1 (format `{rel}:{line}: {text.trim()}`, :949-954) — the full line text is emitted untruncated, so "tail-refreshed" is inside the first 100 displayed lines and the cap can never hide it.
- **Engine marker.** The walk summary line always contains "engine: walk" (:985), and the stale note is merged into the prepended note (:905-911) and rides above the body via `with_note` (:1001).

## 4. No new issues introduced

- `git show 8e90dc0` touches exactly two files: `.coding/reviews/2026-09-17-findings-round-f1-f13-verify-review.md` (the round-2 report, new) and `src/tool/agent/search.rs` (+ one test function, +53 lines). `git status --short` and `git diff HEAD` are both empty; HEAD is 8e90dc0.
- The test is hermetic: `tempdir` fixture with an in-memory graph (`make_indexed_tool`, `search.rs:1018-1022` — no DB file left in the tree), no watcher to interfere (the graph is never re-indexed after the edit), pure `std::fs`/`SystemTime`/serde_json — platform-neutral on both macOS and Windows, no `#[allow]`, nothing that could trip `#![deny(warnings)]` (a test-only addition, all symbols used).
- Documentation sync: a test-only + review-report commit; the test's own comment cites the review round, and no README/PLAN/endpoint-docs surface changes.

## 5. Full-suite arithmetic

Round 1's report pins the baseline "1682 passed, 0 failed" (`.coding/reviews/2026-09-17-findings-round-f1-f13-review.md:36`). Round 2's 1683 = 1682 + exactly one new test function (`co_occurring_markers_each_count`; the other fixes extended existing tests). This commit adds exactly one further test function (`stale_file_beyond_the_display_cap_still_surfaces`) and no source changes, so the post-fix claim of 1684 passed is arithmetically consistent. (As a read-only reviewer I cannot re-run `cargo test`; the consistency claim is verified, matching the established read-only-review convention.)
