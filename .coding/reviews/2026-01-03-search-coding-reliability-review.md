## Verdict: FINDINGS (0 high, 6 low)

**Scope:** ALL uncommitted changes on `wt/agenticcoding` (git diff HEAD + untracked files) for bug-fixing plan 2a78cc3c — "search: reliable .coding/ file location — sticky delegation bypass, empty-index walk fallback, read_files directory listing". Changed: `src/tool/agent/search.rs`, `src/tool/agent/search_read.rs`, `src/tool/agent/read_files.rs`, `README.md`, `PLAN.md`, `.coding/backlog.jsonl`; untracked: `.coding/knowledge/bug/2026-12-31-search-unreliable-for-coding-files-escape-re-del.md`, `.coding/plans/2a78cc3c.md`.

**Summary:** All three fixes are correctly implemented, mirrored across `search`/`search_read`, regression-tested with RED-first tests that exercise the changed paths, and platform-neutral. The deleted/rewritten tests lose no unique coverage; the README/PLAN claims match the new behavior. Six LOW findings: one stale module doc inside the changed file, three stale live knowledge records, dead zero-hit index branches carrying a now-false comment, stale directory hints in `file_edit`, no entry cap on directory listings, and the backlog close-out.

**Method:** full-diff review; changed files read in context (search.rs, search_read.rs, read_files.rs, factory.rs budget test, file_edit.rs, file_read.rs registration); untracked knowledge/plan files read; stale-claim searches run for both target phrases. Test evidence is the main agent's (root 1963 passed / 0 failed / 4 ignored, run twice; src-tauri 186 + 4 doc-tests; frontend untouched) — this reviewer is read-only and did not re-run the suite; the four regression-test bodies were inspected and match the claimed scenarios.


## What was verified (no findings)

### Fix 1 — delegation escape set (search.rs, search_read.rs)

- `delegation_state` is now `Arc<Mutex<Vec<DelegatedKey>>>` with `DELEGATION_SET_CAP = 32` (pub(crate)); recording does retain(dedup) → push → drain-oldest — correct bounded-set semantics; the escape check (`contains(&key)`) gates BOTH the memory arm and the symbol arm in both tools; recording fires on both delegation arms (the fast-path return and the glob-narrowed prepend).
- search_read.rs mirrors the semantics exactly and reuses `search::DELEGATION_SET_CAP` — one shared constant, no drift between the two tools.
- The escape key (pattern+glob+literal) matches the documented contract; a re-issue with a changed glob/literal is correctly a distinct key.
- Concurrency: short critical sections, no await while held; the worst-case race (two concurrent identical queries both delegating before either records) is harmless.
- `delegation_escape_survives_interleaved_queries` (search.rs:2862-2894) pins the exact observed defect scenario with the corrected expectation: A delegates → B delegates → A's re-issue escapes → B's re-issue escapes. The deleted `a_different_delegating_query_replaces_the_escape_key` pinned the removed single-key replacement semantics; its remaining unique asserts (first-time A and B both delegate) are retained in the new test. The memory-arm escape (single query) stays covered by the pre-existing memory-path delegation tests; the set mechanics are arm-agnostic (one check, one recording site).

### Fix 2 — empty-fetch walk fallback (try_index)

- `if total == 0 { return None; }` after the glob-filter loop — an empty fetch AND a starved page both fall through; the dead `raw_len` binding is gone. try_index has exactly two callers (search.rs::execute, search_read.rs::execute — verified via the code graph); both treat `None` as "walk".
- The behavior delta is exactly the fix: pre-fix, an empty fetch proceeded to the F10 freshness check (which only examines files already in the fetched hits) and returned `Hits` with zero hits → the authoritative "no matches found (engine: index)". The starved-page case already walked and still does; F10 is unchanged for non-empty results.
- `index_empty_fetch_falls_through_to_walk_for_new_files` (search.rs:1743+) reproduces the observed defect precisely: index FIRST, then write `.coding/reviews/2026-12-23-security-review.md`, literal `bf0f71c` + glob `.coding/reviews/*.md` must find it. Pre-existing `glob_starved_index_falls_back_to_walk` and `empty_index_falls_back_to_walk` still cover the adjacent cases.
- search_read.rs: `literal_metachar_zero_hits_hint_via_the_walk_fallback` (802-841) preserves the old test's retry-hint intent — hint PRESENT for the metachar zero hit, ABSENT for the metachar-free zero hit — now pinned on the walk path (the only reachable path), plus never `engine: index`.

### Fix 3 — read_files directory listing

- `directory_listing` (read_files.rs:288-318): sorted entries, `name/ (dir)` / `name (file, S bytes, mtime T)`; unreadable entries skipped; an unreadable directory degrades to a per-spec error line — sibling reads survive (backlog #89 intent preserved and pinned by the rewritten test).
- Sandbox validation precedes the `is_dir` branch (read_one_with_count:332) — no traversal; the listing exposes nothing search doesn't already expose.
- Both call shapes covered: the `files`-spec (`directory_spec_returns_directory_hint_not_raw_os_error`, rewritten) and the top-level `path` shorthand (`directory_path_returns_listing` — sorted order + subdir marking pinned).
- The path-shorthand lift (execute:195-207) routes directories through the same `read_one_with_count`; a directory named like a source file cannot misfire the SYMBOL NUDGE (content=None suppresses it).

### Requested checks

1. **Bug-plan requirements — PASS.** Regression tests exercise all three changed paths (above); root cause documented in BUG memory 042531e6 + `.coding/knowledge/bug/2026-12-31-…md` + `.coding/plans/2a78cc3c.md`; no behavior change beyond the three fixes (starved-page walk unchanged, F10 unchanged for non-empty results, backlog/docs are bookkeeping).
2. **Documentation sync — README/PLAN PASS; stale claims remain elsewhere** (findings L1, L2, L4). README.md:52 (empty-fetch guarantee) and README.md:53 + PLAN.md:149-154 (set semantics) accurately describe the new behavior.
3. **Multi-platform neutrality — PASS.** `directory_listing` uses only `std::fs::read_dir`, `DirEntry::metadata`, `modified()`, `duration_since(UNIX_EPOCH)` — cross-platform std; no `cfg(windows)`, no Windows-only APIs; pre-epoch mtimes degrade to 0 rather than panicking.
4. **Deleted/rewritten tests — PASS, no unique coverage lost** (details above).
5. **Budget trim — PASS.** The schema text is accurate ("A directory path returns a sorted listing"; `path` property: "read a file or list a directory") and within the per-filter ceilings enforced by `tools_array_stays_within_context_budget` (factory.rs:1434-1544; ExecutingResearch ceiling 21,100), green in the main agent's runs.


## Findings

**L1 — Stale module doc: search.rs:23-25 still documents the removed single-key semantics.**
The module doc says the escape is "(sticky until a different query delegates)" — the exact semantics this plan removed. Module doc comments are in this project's doc-sync review scope, and this one sits in the primary changed file. Fix: one clause, e.g. "(sticky per exact query — a bounded set of bypassed keys; interleaved delegating queries never break an earlier escape)".

**L2 — Three live knowledge records still document the old escape semantics.**
- `.coding/knowledge/decision/2026-12-28-search-search-read-auto-delegation-sticky-escape.md:6` — "(1) STICKY ESCAPE KEY: the last delegated query … is stored on the tool (Arc<Mutex<Option<DelegatedKey>>>); … the key persists until a DIFFERENT query delegates".
- `.coding/knowledge/spec/2026-12-29-search-search-read-auto-delegate-symbol-memory-l.md:7` — "sticky until a different query delegates (no delegate/escape ping-pong)".
- `.coding/knowledge/how/2026-12-28-read-the-auto-delegated-block-first-graph-contex.md:7` — "(the escape key is sticky until a different query delegates)".

These are live records that feed memory recall into future sessions (the DECISION is auto-recalled — it surfaced during this very review). Per the project's memory-hygiene rule (supersede/amend when a fact changes; never leave contradicting records live), amend them — or supersede the DECISION — when landing. The historical plan/review files (`.coding/plans/169046c7.md`, the 2026-12-28 review reports) are dated records and fine to leave as-is.

**L3 — Dead zero-hit index branches + a now-false comment (search.rs:1287-1299; search_read.rs:394-396, 404-419).**
Post-fix, `try_index` returns `FtsOutcome::Hits` only after `if total == 0 { return None; }`, and whenever total ≥ 1 the first glob-passing hit is always pushed (hits.len()=0 < MAX_MATCHES) — so `ix.hits.is_empty()` can never be true in either caller. The `if ix.hits.is_empty()` arms are unreachable dead code, and the search_read.rs comment ("an empty raw fetch from the index is authoritative, so this branch is reachable exactly like the walk branches") is now factually false, as is the build_index_output doc comment's "the zero-hit branch" phrasing. The C2 retry-hint behavior is preserved and pinned on the walk path (search.rs `literal_metachar_zero_hits_get_the_retry_hint`; search_read.rs `literal_metachar_zero_hits_hint_via_the_walk_fallback`), so the dead arms can be removed outright — which also eliminates the last producer of "no matches found … (engine: index)", making the new tests' `!contains("engine: index")` assertions structural. Minimal alternative: correct the two comments.

**L4 — file_edit directory hints point at the demoted enumeration path (file_edit.rs:866, :926).**
Both the execute and approval-preview directory guards say "'{}' is a directory, not a file — use search with a glob like '{}/*' to list its files" — the exact search-glob gymnastics this plan demoted (cap + truncation) when read_files now lists the directory directly. Update to point at read_files (e.g. "— read_files on '{}' returns its listing"), updating the tests that assert this message. file_read.rs:131 carries the same text but FileReadTool is not registered in the factory (test-only), so it is unreachable in production — optional. Folding this into the commit is a one-line-each doc-sync fix; queueing it as a follow-up is also acceptable if you want this commit strictly limited to the three fixes.

**L5 — directory_listing has no entry cap (read_files.rs:288-318).**
A huge directory (target/, node_modules/ — 10k+ entries) produces a listing bounded only by TOTAL_BYTE_CAP (execute:271-274): up to ~512KB of listing, truncated mid-list. The header's true entry count plus the truncation note do signal incompleteness, but a per-listing entry cap (a few hundred entries + "… and N more entries" note) would mirror search's MAX_MATCHES discipline and protect the conversation budget — the same reliability rationale that motivated this plan.

**L6 — Backlog close-out: item 92a41825 is still `in_flight`.**
When landing, flip it to `done` with a resolution note (date, plan 2a78cc3c, commits) per the established pattern (cf. c8e48f81) — via backlog_status before/with the commit, so it doesn't dangle.


## Observations (no action required)

1. **This reviewer's own tool surface runs the pre-fix binary** — a live literal search for `delegat` (glob README.md) returned "no matches found (engine: index; 1901 files indexed)" although README.md contains "delegation": the exact defect-2 behavior, from the compiled process predating the change. Under the fix, that empty FTS fetch falls through to the walk and matches. Two takeaways: the defect is real (independently reproduced), and the fix also heals partial-token literal searches (a token-exact FTS miss yields an empty fetch → walk). Nothing to do — just don't read this session's search behavior as post-fix.
2. **Residual, pre-existing semantics gap (unchanged, out of scope):** a literal whose FTS fetch is NON-empty can still miss substring-only occurrences in other files (FTS matches whole tokens; the walk matches substrings) — e.g. a literal `gate` finds files containing the word "gate" via the index but misses files containing only "delegate". The empty-fetch fallthrough fixes the zero-fetch case; the non-empty-but-incomplete case remains, is acknowledged by the tool's LITERAL TIP ("an FTS phrase and a regex are not semantically equivalent"), and is untouched by this diff. Flagged for awareness only.
3. `.lock().unwrap()` on the delegation mutex would panic on poisoning — a pre-existing pattern (the old single-key code did the same), no await while held, not a new risk.

**Bottom line:** the three fixes are sound and well-tested; the findings are documentation/hygiene items (L1-L4), one robustness nit in the new listing (L5), and the backlog close-out (L6). Fix L1-L3 before committing (they touch the changed files' own docs/comments and dead code); L4-L6 are one-liners or bookkeeping the main agent can fold in or queue.
