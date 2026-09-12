# Review — feat/search-content-index (plan 7a86564c, uncommitted diff vs HEAD)

Scope: all 24 changed files in `git diff HEAD` plus the new **untracked** `src/tool/agent/pattern.rs` (read in full; it does not appear in `git diff HEAD`). `.coding/backlog.json` / `.coding/plans/stack.json` changes are expected bookkeeping, ignored per instructions.

Verified clean: `match_lines` offset→line mapping (partition_point on line_starts — correct, no off-by-one; blank lines keep their numbering in `upsert_file` via `enumerate`), transaction atomicity (content + meta rows share the symbol-upsert tx; `remove_file` drops them in the same tx), backfill steady-state (meta written on every upsert incl. empty content → fires once per old file, `files_reindexed == 0` next pass), reindex concurrency (flag check+set within one non-yielding poll segment, same F1 argument as `codegraph_refresh`; panic → JoinError → Failed event), event throttling (shared `should_forward_progress`/`PROGRESS_EVERY`), and all TS wire shapes (`ReindexEvent` pinned by `reindex_event_wire_shape`; `OpEvent` widening is backward-compatible; `searchResultInfo` handles both engines' summary forms, the `search_read` `; reading top N` variant, and strips the note before scanning for `engine:`).

## Correctness

### C1 (HIGH) — Index engine silently misses every file type the codegraph doesn't parse (false "no matches found" + silently incomplete results)
- `src/tool/agent/search.rs:284-314` (index gate), `src/tool/agent/search.rs:167-199` (`try_index`), `src/tool/agent/search_read.rs:138-144`, root cause: `src/codegraph/walk.rs:28-35` + `walk.rs:79-84` — `walk_project` only yields `.rs` / `.ts` / `.tsx` (`Lang::from_extension`), so `cg_content` contains **only those three extensions**, while the walk engine searches every text file (`.md`, `.json`, `.toml`, `.py`, `.css`, `.html`, …).
- Because `try_index` returns `Some` whenever the index is merely populated, a literal single-line query is answered authoritatively from a partial index with **no walk fallback**:
  - No glob: a term appearing in `.rs` and `.json` reports only the `.rs` hits — the "N matches in M files (engine: index)" total is silently wrong. A term appearing only in docs/config → `no matches found (engine: index)` — a false negative the agent will act on (concludes the string doesn't exist).
  - Glob naming a non-indexed type (`**/*.md`): FTS returns only rs/ts/tsx rows, the glob post-filter drops all of them → guaranteed false `no matches found (engine: index)` even when matches exist.
- This contradicts the module docs ("everything else … walks the tree", "transparent") and the tool schema ("served by the full-text content index when populated"), which imply equivalence. The parity test (`index_matches_walk_on_large_tree`, search.rs:607-642) uses only `.rs` files, which is why it passes.
- Fix (pick one): (a) content-index **every** file `should_search` accepts (content-only rows for non-parseable files) so index ≡ walk — the honest fix that keeps the no-glob fast path; or (b) at minimum, `try_index` returns `None` when post-glob hits are empty (fall back to walk before ever asserting "no matches") **and** the index path is gated on a glob that provably restricts to indexed extensions. Add a regression test with a non-indexed extension asserting index == walk results.

### C2 (MEDIUM) — Zero-token literals return a false "no matches found (engine: index)"
- `src/codegraph/store.rs:509-514` + the gate at `search.rs:284`. A literal like `->`, `::`, `&&`, `**`, or `(` passes the `escaped.is_empty()` guard but tokenizes (unicode61) to **zero tokens**, so the phrase matches nothing → `no matches found (engine: index)` while the walk engine matches thousands of lines. Same class as C1 but independent of file coverage (fails even inside `.rs` files).
- Fix: in `try_index`, if the pattern contains no tokenizable characters (no unicode alphanumeric/word chars after trim), return `None` → walk.

### C3 (LOW/MEDIUM) — Glob post-filter can starve past the 5× over-fetch
- `src/tool/agent/search.rs:176-198`. `search_content` returns the top 500 by bm25, then the glob filter runs client-side. If more than 500 better-ranked hits fail the glob, glob matches beyond rank 500 are invisible: the reported total undercounts (at least the "... and more matches" note fires since `total > hits.len()`), and in the all-500-filtered case the tool asserts a false "no matches found". The 5× margin makes this rarer than C1/C2 but not impossible on large repos.
- Fix: when post-glob `total == 0` but the raw fetch was non-empty, return `None` → walk (walk then produces the authoritative answer, including a true "no matches").

## Bugs (low)

### B1 (LOW) — "matched literally" note is inaccurate on the index engine
- `src/tool/agent/pattern.rs:63` says "matched literally instead", and the index path is taken when the fallback fired (`search.rs:284`, `search_read.rs:138`). On the index engine a broken regex like `foo(` is actually matched as the FTS token `foo` — matching lines that contain `foo` but not `foo(`. The note (also rendered verbatim in the agent UI's fallback-note line) promises literal semantics the engine doesn't deliver. Fix: either route fallback-fired queries to the walk engine, or extend the note when `engine: index` ("matched as literal text; tokenized by the index").

### B2 (LOW) — `file_edit` invalid-regex error lost its argument context
- `src/tool/agent/file_edit.rs:481-485`: the message was `invalid regex in old_string: {e}`, now the generic `invalid regex: {e} — …`. Only one regex-bearing arg exists so impact is small, but prefixing `in old_string` costs nothing and helps the model. The new actionable "retry with literal matching" tail is good.

## Security — no findings
- SQL: fully parameterized (`MATCH ?1`, `LIMIT ?2`); the pattern never reaches SQL text.
- FTS injection: `search_content` phrase-quotes with internal quotes doubled (store.rs:510-514) — the whole pattern is one quoted phrase, so FTS operators (`AND`/`OR`/`NEAR`/`*`) cannot be injected. Pinned by `search_content_empty_and_quote_safe`.
- Paths: the glob post-filter runs on DB-relative `/`-separated paths produced by our own walker; `read_one` re-validates against the sandbox; the reindex command takes no path input. `validate_glob` (abs/drive/UNC/`..`) still runs before everything.

## Resources — no findings
Progress throttled; FTS queries bounded (≤500 hits); `has_content` added inside the existing per-file lock block (one lock, two queries); panic paths emit terminal `Failed` so the UI bar can't wedge.

## Constitution compliance — compliant, one process note
- Doc comments present on all new public items (`pattern.rs`, `ContentHit`, `Store::has_content/search_content/content_coverage`, `CodeGraph::search_content/content_coverage`, tool constructors, `ReindexEvent` + fields).
- No `#[allow]` added — the only two hits (`src/agent/loop_impl.rs:344,384`) are pre-existing in an untouched file.
- Regression tests present for every fixed defect (invalid-regex fallback in both tools, multi-line CRLF in `pattern.rs` + both tools, backfill + steady-state, re-upsert content replace, remove-file content drop, quote-safe FTS, wire-shape pin).
- Line endings preserved; git's "CRLF will be replaced by LF" notices are this repo's usual autocrlf checkout noise, matching untouched files.
- **Process note (must fix before commit): `src/tool/agent/pattern.rs` is new and UNTRACKED — it is absent from `git diff HEAD` and must be explicitly `git add`ed (e.g. via `git add -A`) or the commit won't compile.**
- Test-gap note: add a non-indexed-extension case to the index-vs-walk parity test when fixing C1.

## Summary
The regex-resilience module, CRLF handling, FTS schema/population/backfill, transaction atomicity, reindex event stream, and frontend wiring are all sound. The one design-level defect is C1: the index engine is not equivalent to the walk engine over the project's files (Rust/TS/TSX only), producing silent false negatives for literal queries that touch docs/config/other file types — recommend fixing before merge. C2/C3 are the same failure class (index answers "no matches" without walk verification) and share the "fall back to walk on empty post-glob result" mitigation.
