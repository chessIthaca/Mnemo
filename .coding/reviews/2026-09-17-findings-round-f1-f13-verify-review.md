## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of the four round-1 findings (report `.coding/reviews/2026-09-17-findings-round-f1-f13-review.md`) against commit 8974dda on `wt/agenticcoder`. The tree is clean (`git status --short` empty), so working-tree content == the commit. Findings 1, 2, and 4 are genuinely fixed, correctly, with regression tests that fail on the pre-fix code. Finding 3 is correctly fixed in code, but its widened whole-page scan lacks a discriminating regression test (the existing F10 test passes on the pre-fix `hits`-scoped code) — one low finding.

## Per-finding verification

### Finding 1 — multi-marker counting: FIXED ✓

`src/agent/steering_stats.rs`:
- `observe_result` (`:326-349`) now detects with `MARKERS.iter().copied().filter(|k| k.detect(...))` and increments the fired counter for EVERY detecting kind (`:336-338`) — first-match-wins is gone. `last_fired` prefers the most recently fired ACTIONABLE kind via `detected.iter().rev().find(|k| !k.targets().is_empty())` (`:339-348`); `targets()` (`:105-126`) correctly marks LiteralTip/KnownMemoryHit/ConsolidationDue fired-only, so a dispatcher-appended consolidation note riding a search-nudge result can no longer evict the actionable note, and an all-fired-only result still consumes the note via `detected.last()` (`:346`).
- The MARKERS doc comment (`:223-230`) now documents co-occurrence and cites the review.
- Test `co_occurring_markers_each_count` (`:448-485`) + `count_of` helper (`:440-446`): case (a) exercises the exact round-1 scenario — a uuid-shaped regex search first line carrying the literal TIP joined with the known-memory note ("; ") — asserting BOTH `literal-tip` and `known-memory-hit` fired == 1 (the old `find` counted only the TIP; this test fails on the pre-fix code). Case (b) covers search-nudge + consolidation-due co-occurrence, asserts both counters, and — via `observe_call(1, "graph_context")` → `search-nudge` switched == 1 (`:482-484`) — proves the actionable note survives the fired-only kind.

### Finding 2 — atomic once-per-session gate: FIXED ✓

`src/tool/steering.rs` `consolidation_due_note` (`:86-135`): the reservation is inserted BEFORE the count query (`:96-104`; `HashSet::insert` returning false → silent `None`, `:101-103`), and released again when the count stays below the threshold or the store errors (`:122-133`, both the `Some(n) if n >= THRESHOLD` mismatch and the `None` from `.ok()`). Hold-and-decide, exactly as suggested. Poison handling present at both lock sites; no `.await` while holding the gate lock (the guard is dropped before `list_by_tier`, `:104-107`) — no deadlock surface. Doc comments updated (`:80-85`, `:92-95`) citing the review.
- Test `consolidation_note_fires_once_at_the_threshold` (`:474-513`) genuinely exercises the new release path: the first (below-threshold) call inserts the reservation, queries, and releases it — if the release were missing, the second (at-threshold) call would return `None` and fail its `.expect("crossing the threshold fires the note")` (`:503`). The third call proves the fired gate holds. (The interleaving race itself is not deterministically testable; this is the correct testable slice.)

### Finding 3 — whole-page freshness scan: FIXED in code ✓, coverage gap (see low finding)

`src/tool/agent/search.rs` `try_index`: the `files` set is populated for EVERY glob-passing hit on the fetched page before the display cap (`:597` insert, `:598-600` cap), so the F10 freshness loop (`:621-635`) now compares mtimes for all distinct hit files — including matches beyond `MAX_MATCHES` that only feed the `total`/`files` summary counts — and any mismatch returns `FtsOutcome::Stale` → caller falls through to the walk and surfaces the staleness (`:893-911`). The comment (`:611-618`) documents the widened scope and cites the review. The fix is correct and matches the suggested "probe the full page before capping" direction.

### Finding 4 — CRLF preservation on pointer rewrite: FIXED ✓

`src/tool/memory/mod.rs` `reconcile_file_pointers` (`:809-816`): `sep` is chosen by `body.contains("\r\n")`, the lines are rejoined with that separator, and the trailing newline (when `body.ends_with('\n')`) is re-added in the same style — a CRLF body keeps CRLF, a LF body keeps LF. Doc comment (`:809-811`) cites the review.
- Test: the CRLF case appended to `reconcile_file_pointers_rewrites_only_stale_leading_pointers` (`:1271-1288`) rewrites the stale pointer in place, asserts the rewritten line ends `\r\n`, the trailing `\r\n` is preserved, and no mixed endings (`\r\n\n` / `\n\r`) are introduced. This fails on the pre-fix LF-normalizing code.

## Low finding

### L1 — Finding 3's whole-page widening has no discriminating regression test

The code fix is correct, but no test fails on the pre-fix `hits`-scoped scan. The only F10 staleness test, `stale_index_serves_fresh_walk_results` (`src/tool/agent/search.rs:1986-2015`), uses a single displayed file — it passes on the old code too, so the exact defect (a stale file whose matches land beyond the display cap, leaving the "N matches in M files (engine: index)" summary stale) could silently reappear. I checked the index-mode tests: none constructs >`MAX_MATCHES` index hits plus a stale file beyond the cap (`index_matches_walk_on_large_tree` at `:1335-1369` has 40 files, all fresh; `caps_results_at_max_matches` at `:1432-1467` is walk-mode). Per the constitution (regression test must fail without the fix), add one — e.g. 100+ index files with strong matches plus one weaker-match file edited after indexing (bump mtime), asserting the search surfaces "index stale" and walk results.

## Sanity notes

- Full-suite claim (1683 passed, 0 failed) is arithmetically consistent: round 1 reported 1682 with the round's tests; the fix set adds exactly one new test function (`co_occurring_markers_each_count`) — the other fixes extended existing tests (F2's `consolidation_note_fires_once_at_the_threshold`, F4's CRLF case).
- No new issues introduced by the fixes: the actionable-preference logic in finding 1 is sound (tool-scoped detection means at most one actionable kind per result, so `rev().find` is deterministic); finding 2's gate lock never spans an `.await`; finding 3 fails toward the walk (missing cg_files row / unreadable mtime ⇒ stale, the safe direction); finding 4's rejoin preserves non-pointer lines byte-for-byte modulo the style separator (the round-1-approved `contains("\r\n")` heuristic).
