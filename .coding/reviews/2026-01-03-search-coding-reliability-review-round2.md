## Verdict: PASS

**Scope:** Round-2 verification of commit 79aaadd (HEAD of `wt/agenticcoding`, confirmed via git log; parent 1a0e8a4) for bug-fixing plan 2a78cc3c — the six round-1 LOW findings (.coding/reviews/2026-01-03-search-coding-reliability-review.md). Working tree: clean except `.coding/backlog.jsonl` (the L6 done-flip, uncommitted — see L6), exactly as the task anticipated.

**Summary:** All six round-1 findings are resolved and the fixes introduced no new issues. Every requested check passed: the `IndexHits.indexed_files` removal broke no consumer, the `build_index_output` signature is consistent at its single call site, the listing cap logic is correct at its boundaries, and all four stale-text sweeps come back clean. One state nuance (not a defect): the L6 backlog done-flip lives in the working tree, not inside 79aaadd — it must ride the final commit.

**Method:** Round-1 report read in full; committed state verified by reading the changed regions directly (tree clean except backlog.jsonl ⇒ on-disk = 79aaadd for every file reviewed); git log/diff/status for state; literal-text sweeps for all four stale-claim classes; memory_search for the BUG record; backlog_list + on-disk backlog.jsonl for L6. Tests NOT re-run (read-only reviewer) — test evidence is the main agent's, with all five regression-test bodies inspected against the changed code.


## Round-1 findings — all six resolved

**L1 (stale module doc) — RESOLVED.** search.rs:22-27 now reads "…skips delegation and runs the plain file search (sticky per exact query — a bounded set of bypassed keys; interleaved delegating queries never break an earlier escape)" — the exact clause round 1 prescribed. Sweep for "sticky until a different query delegates": zero hits in src/ and live docs (only dated plan/review files, which round 1 explicitly sanctioned as historical).

**L2 (three stale knowledge records) — RESOLVED.**
- DECISION superseded properly: new `.coding/knowledge/decision/2026-12-31-search-search-read-auto-delegation-bounded-bypas.md` (frontmatter `supersedes = "2026-12-28-search-search-read-auto-delegation-sticky-escape"`) documents the bounded set — cap 32, dedup, oldest-evict, keys never displaced, with the observed-defect rationale; the old 2026-12-28 file carries `status = "superseded"` (line 4). Its body keeps the historical single-key text, which is correct for a superseded record (excluded from default recall, not rewritten). Live recall confirms the new record is the one that surfaces: a memory-targeted search this session recalled the bounded-set DECISION as hit 1.
- SPEC amended in place (`.coding/knowledge/spec/2026-12-29-…md`): the escape-hatch clause now reads "every delegated query's DelegatedKey (pattern+glob+literal) joins a bounded set on the tool (cap 32, dedup, oldest evicted); a repeat of ANY recorded key skips delegation and runs the plain search — sticky per exact query, never displaced by interleaved delegating queries (no delegate/escape ping-pong)".
- HOW amended in place (`.coding/knowledge/how/2026-12-28-…md`): "the bypass is sticky per exact query — a bounded set of escaped keys, never displaced by other delegating queries; 2026-01-03, plan 2a78cc3c".

**L3 (dead zero-hit index branches) — RESOLVED.**
- search.rs: the `if ix.hits.is_empty()` arm is gone; the Hits arm (:1284-1306) renders directly, with the invariant comment "try_index returns Hits only with total ≥ 1 (an empty fetch walks — 2026-01-03), so the hits are never empty here; the C2 metachar retry hint lives on the walk path".
- try_index (:842-924): `if total == 0 { return None; }` (:877-890) with the two-case comment (C3 starved page + empty fetch during watcher reindex lag); the fn doc (:832-841) updated to match; `IndexHits` constructed without `indexed_files` (:919-923); `content_coverage` destructured `(_, with_content)` (:848).
- search_read.rs: `build_index_output` (:397-434) lost the `pattern` param and the dead empty-hits block; its doc comment states the invariant; the single caller (:272) passes the new 4-arg shape consistently.
- No other consumer broke: "indexed_files" survives only as the unrelated `CodeGraph` store method (src/codegraph/store.rs). "engine: index" producers are now exactly the two non-zero-hit summaries (search.rs:1295, search_read.rs:422) — no zero-hit producer remains, making the tests' `!contains("engine: index")` assertions structural, as round 1 suggested.

**L4 (file_edit/file_read directory hints) — RESOLVED.** file_edit.rs:866 and :926 (execute + approval preview) and file_read.rs:131 all say "'{}' is a directory, not a file — read_files on '{}' returns its listing". Sweep for "use search with a glob like": zero hits in src/. The test assertions (file_edit.rs:1072, file_read.rs:246) assert only the unchanged prefix, so nothing dangles.

**L5 (no entry cap on directory listings) — RESOLVED.** `LISTING_ENTRY_CAP = 300` (read_files.rs:282-286, with rationale doc); `directory_listing` renders `entries.iter().take(LISTING_ENTRY_CAP)`, the header keeps the TRUE count, and `total > cap` appends "… and N more entries (narrow with a more specific path)" (:317-329). Boundary behavior correct (300 → all shown, no note; 301 → 300 shown + "and 1 more entries"); entries are sorted before the take, so the cap is deterministic. New test `directory_listing_caps_entries` (350 files): asserts "(directory, 350 entries)" header, f349 hidden, "and 50 more entries" — red pre-cap by construction. 300 short entry lines ≈ 15-18 KB, comfortably inside TOTAL_BYTE_CAP; the round-1 flood concern is closed.

**L6 (backlog close-out) — RESOLVED in substance; the flip is uncommitted (expected).** Item 92a41825 is `done` in the live backlog store and in the on-disk backlog.jsonl, with the resolution note "Resolved by plan 2a78cc3c (commit 79aaadd on wt/agenticcoding): … Round-1 review 0 high / 6 low — all fixed in 79aaadd; round-2 verification in flight. Root 1964/0/4, src-tauri 186+4/0." — the established c8e48f81 pattern. Nuance, not a defect: 79aaadd itself contains only the pending→in_flight transition; the done-flip is the sole uncommitted working-tree change and must ride the final commit (the closing-sequence commit that also lands this report). Do not let it dangle.


## New-issue checks — all clean

1. **IndexHits.indexed_files removal:** no consumer anywhere (literal sweep: only the unrelated `CodeGraph::indexed_files` store method in src/codegraph/store.rs); the green `#![deny(warnings)]` build structurally confirms no dead references.
2. **build_index_output signature change:** single caller (search_read.rs:272), 4 args matching the definition at :397-402; no other callers (code-graph + literal sweep agree).
3. **Cap logic:** `take(LISTING_ENTRY_CAP)` + remainder note + true-count header — correct at the boundaries; sorting precedes the take, so the shown set is deterministic.
4. **Stale text:** all four requested sweeps clean — "sticky until a different query delegates" (only dated plan/review files), "use search with a glob like" (only the round-1 report quoting its own finding), "indexed_files" in tool modules (none), zero-hit "engine: index" producers (none — both remaining producers sit behind `total ≥ 1`).
5. **Delegation-set mechanics (direct read):** `DELEGATION_SET_CAP = 32` pub(crate) (search.rs:341-346, doc includes the eviction-is-harmless rationale); `DelegatedKey` = pattern+glob+literal, Clone + PartialEq (:361-386); field `Arc<Mutex<Vec<DelegatedKey>>>` with updated doc (:398-404); the escape check gates BOTH the memory arm and the symbol arm in BOTH tools (search.rs:1172-1173/:1221, search_read.rs:175-176/:209); recording on both delegation arms does retain(dedup) → push → drain-oldest (search.rs:1240-1246, search_read.rs:228-234), with search_read reusing `search::DELEGATION_SET_CAP` — one shared constant, no drift. The deleted single-key test (`a_different_delegating_query_replaces_the_escape_key`) left no references ("escape_key" sweep: zero hits in .rs).
6. **No behavior drift beyond the three fixes:** the C3 starved-page walk and the F10 staleness path are unchanged for non-empty results; the C2 retry hint lives on the walk path, pinned by `literal_metachar_zero_hits_get_the_retry_hint` (search.rs) and `literal_metachar_zero_hits_hint_via_the_walk_fallback` (search_read.rs).

## Test evidence (main agent's; not re-run — read-only reviewer)

Root 1964 passed / 0 failed / 4 ignored (round 1: 1963 — the +1 is `directory_listing_caps_entries`); src-tauri 186 passed + 4 doc-tests; frontend untouched (no TS changes in the change set). All five regression tests are present with bodies matching the claimed scenarios: `index_empty_fetch_falls_through_to_walk_for_new_files` (index FIRST, then write the .coding/reviews file, literal+glob must find it — red pre-fix), `delegation_escape_survives_interleaved_queries` (A delegates → B delegates → A escapes → B escapes — the exact observed defect, red pre-fix), `walk_cap_narrower_reissue_returns_tail_files` (pin, disclosed as such in both the plan and the BUG record), `directory_path_returns_listing` (red pre-fix), `directory_listing_caps_entries` (red pre-cap). With `#![deny(warnings)]` at both crate roots, the green run is also proof of zero warnings/dead code.

## Bug-plan requirements — PASS

Regression tests exercise all three changed paths (verified by reading the test bodies against the changed code); root cause documented in BUG memory 042531e6 + the committed knowledge file (.coding/knowledge/bug/2026-12-31-search-unreliable-for-coding-files-escape-re-del.md: symptom → root cause with line refs → fix → regression-test names) + the committed plan file (.coding/plans/2a78cc3c.md); the round-1 report is committed (no untracked files remain in the tree).

## Observations (no action required)

1. **The defect reproduced live during this review, from the pre-fix binary:** a literal search for `LISTING_ENTRY_CAP` returned "no matches found (engine: index; 1901 files indexed)" although the const demonstrably sits at read_files.rs:286 (read directly). Same class as round 1's `delegat` observation — independent confirmation the empty-fetch false negative was real, and that the fix also heals partial-token literal misses.
2. **Pre-existing, untouched by this diff:** the doubled "note: note:" prefix when a staleness note rides `with_note` (the stale_note string already begins "note: " and with_note prepends another) — visible throughout this session's search outputs; F10-era code, out of scope here.
3. The old DECISION file keeps its historical single-key body behind `status = "superseded"` — correct per the memory-hygiene convention; the new bounded-set DECISION is what recall surfaces.

**Bottom line:** all six round-1 findings verified resolved in/around 79aaadd; no new issues introduced. Proceed with the closing sequence — and make sure the final commit includes the uncommitted backlog.jsonl done-flip along with this report.
