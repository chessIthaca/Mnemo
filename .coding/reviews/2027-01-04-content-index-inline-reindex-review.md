## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes on `wt/agenticcoding` (git diff HEAD + untracked) for plan a00d05fc "Content-index staleness: inline reindex instead of warn + single 'note:' prefix". Changed files: `src/codegraph/mod.rs` (+184), `src/tool/agent/search.rs` (+301/-55), `src/tool/agent/search_read.rs` (+67/-1), `README.md`, `PLAN.md`, `.coding/backlog.jsonl`; untracked `.coding/plans/a00d05fc.md` (plan file) + `.coding/knowledge/bug/539e16c4.md` (unrelated R14 bug record).

The implementation is correct, well-tested, and the doubled-"note:" bug is genuinely fixed. The retry loop is exactly one bounded retry with no infinite-loop path; watcher coordination via `compare_exchange` + `IndexFlagGuard` is sound (no deadlock, no double-clear, busy claim leaves the flag intact); every output path carries at most one "note: " prefix; and the regression tests cover each behavior change (single prefix, reindex→fresh-index serve, walk above cap, mid-pass walk, busy-flag, prune, refresh). Two low-severity findings below; neither blocks merge.

### LOW 1 — `reindex_stale_files` trusts its `rel_paths` input without a containment check (defense-in-depth)

`CodeGraph::reindex_stale_files` (src/codegraph/mod.rs:392) does `let abs = self.root.join(rel); std::fs::read(&abs)` for each caller-supplied rel path, with no verification that `rel` stays under `self.root`. By contrast, `CodeGraph::index` only ever processes paths yielded by `walk_searchable(&self.root)` and stores them via `relpath(root, path)` (`strip_prefix` — guaranteed under-root).

Verification of the actual risk (the review brief asked specifically):
- **No filesystem mutation outside root.** `Store::remove_file(rel)` and `Store::upsert_file(rel, …)` operate on the DB key `rel` (cg_files / cg_content rows), not on filesystem paths. A crafted `rel` cannot delete or write a file outside the project.
- **The only caller passes trusted paths.** `reindex_stale_files` is called solely from `try_index` (search.rs:983) with `page.stale_paths`, which are built in `fts_page` from `ContentHit.path` (cg_content rows) and `stored_mtimes()` keys (cg_files rows) — both written by the indexer through `relpath`/`strip_prefix`. Under normal operation no `..` key can exist.
- **Residual gap.** A `../`-prefixed DB key (only reachable by tampering with the local `<root>/.coding/codegraph.db` cache) would make `std::fs::read(&abs)` read a file outside root and index its content into the FTS (information disclosure into search results). This requires local DB write access — at which point the attacker owns the machine — so it is not an exploitable vulnerability, only a defense-in-depth gap. The pre-existing freshness check in `fts_page` already did `std::fs::metadata(root.join(path))`, but `reindex_stale_files` newly adds a full `std::fs::read` + upsert on the joined path.

Suggested hardening (non-blocking): for each `rel`, assert `self.root.join(rel).canonicalize()` starts with `self.root.canonicalize()` (or re-derive via the existing `relpath(&self.root, &abs)` and skip on `None`), so a crafted key can never read outside-root content. This matches the containment `index()` gets for free from the walker.

### LOW 2 — No DECISION/SPEC memory record for this behavior evolution (bookkeeping)

The change is a durable design decision (inline reindex ≤ 8 stale files + one re-query, walk above cap / while a pass runs, single-prefix note) that supersedes the prior warn-only F10 contract. The living docs are synced — `README.md:52`, `PLAN.md:958-961`, the `reindex_stale_files` / `try_index` / `FtsOutcome::Stale` / `IndexHits.reindexed` / `push_note` / `STALE_REINDEX_CAP` doc comments, and the call-site intro comments all describe the new behavior; the `search`/`search_read` `description()` strings never mentioned staleness, so nothing was stale there. However the historical SPEC record `.coding/knowledge/spec/2026-08-29-findings-round-f1-f13-targeted-reindex-supersede.md:6` still describes F10 as "→ walk fallback + 'content index stale for N file(s)' note" (warn-only). That file is a point-in-time historical record (dated 2026-09-17) and should NOT be edited in place, but per the memory_write-on-learning rule ("the moment you learn something durable… Deferred = lost") a DECISION memory should be written at finish so future recalls don't resurrect the warn-only behavior. Flagging so it isn't dropped at the closing sequence.


## Per-check-area verification

### 1. Retry-loop correctness ✓
`try_index` (search.rs:963-994) is a `for attempt in 0..2` loop:
- attempt 0: `fts_page?`; empty `stale_paths` → `Hits(reindexed=0)`; stale ≤ `STALE_REINDEX_CAP` AND `reindex_stale_files(..).is_ok_and(|n| n>0)` → `reindexed = stale_paths.len()`, `continue`; else → `Stale`.
- attempt 1: `fts_page?`; empty → `Hits(reindexed)` (carried from attempt 0); else `attempt==0` is false so the reindex branch is skipped → `Stale`.

Exactly one bounded retry — no infinite loop even if a file is edited every tick (attempt 1 never re-reindexes). Staleness above cap walks (`stale_paths.len() <= CAP` fails → Stale). A reindex that empties the result set: on attempt 1 `fts_page` hits `total == 0 → return None`, the `?` propagates `None` → walk, no note — matches the documented intent (search.rs:961-962).

**`reindexed` count is accurate in every reachable Hits path.** I traced each failure branch of `reindex_stale_files`: a file that fails to refresh (read-error + `remove_file` error, or `upsert`/`reindex_one` error) leaves its DB rows unchanged, so its mtime still mismatches on attempt 1 → `Stale` (no disclosure). A vanished file that prunes is counted in both `stale_paths.len()` and `refreshed`. A successfully-reindexed file that drops off the attempt-1 hit page (content no longer matches) was still refreshed and counted. So `reindexed = stale_paths.len()` equals the actual refreshed count whenever `Hits` is reached — no inflation. (No finding.)

### 2. Watcher coordination ✓
`reindex_stale_files` (mod.rs:392-460): `indexing.compare_exchange(false, true, Relaxed, Relaxed)` — on `Err` (flag already true) returns `Ok(0)` **without touching the flag** (busy claim leaves someone else's flag intact — pinned by `reindex_stale_files_busy_returns_zero_and_leaves_the_flag` and `stale_while_an_index_pass_runs_walks`). On success, `IndexFlagGuard(&self.indexing)` holds the flag and clears it on drop — every exit path including the `rebuild_edges()?` propagation (the `?` returns from the fn, dropping the guard) and a panic unwind. Same guard/lifecycle as `index()`. No deadlock: the watcher's debounce loop (watcher.rs:202) only `while is_indexing() { sleep }` — it holds no lock, so an inline reindex holding the flag cannot block it; the store `Mutex` is taken per-file (short critical sections) in both, and the flag makes the passes mutually exclusive. No double-clear: only one `compare_exchange` claim can succeed, so only one guard exists. `Ordering::Relaxed` is consistent with `index()`'s existing `store(true, Relaxed)` — the flag is a coarse hint; real synchronization is the store `Mutex` + the polling loop. (Pre-existing, not introduced here: a tiny window between the watcher's `while`-exit and `index()`'s unconditional `store(true)` could let an inline reindex slip in — but `index()` itself doesn't use `compare_exchange`, so this race class predates this change and is bounded by atomic per-file transactions. Not a new finding.)

### 3. Note merging ✓ (the doubled-"note:" bug is fixed)
`with_note` (search.rs:998) prepends exactly one `"note: "`. `push_note` (search.rs:1009) joins with `"; "` and adds NO prefix; its doc states the component must not start with `"note: "`. Verified every output path:
- **search.rs Hits arm** (1393-1404): `push_note(note, "reindexed N stale file(s) — serving fresh index results")` — no prefix — then `with_note(body, &note)`. Single prefix.
- **search.rs Stale arm** (1407-1409): string is now `"content index stale for {files} file(s) — serving tree-walk results"` (the old leading `"note: "` is dropped); merged via `push_note` (1420); `with_note` adds the single prefix at the walk return (1519). Single prefix.
- **search_read.rs Hits arm** (278-289): same `push_note` + `build_index_output`→`with_note`. Single prefix.
- **search_read.rs Stale arm** (292-294, 301): same drop + `push_note` + `with_note`. Single prefix.
- `merged_note` components (delegation block "AUTO-DELEGATED…", fallback "matched literally", nudge "'X' is an indexed symbol…", tip "TIP: …", memory "known memory hit: …") none start with `"note: "`. A delegation block + reindex disclosure can co-occur (symbol-hunt-with-glob + literal index hit) → `"note: AUTO-DELEGATED…; reindexed N…"` — still one prefix. Reindex disclosure (Hits arm) and staleness note (Stale arm) are mutually exclusive. Tests assert `!contains("note: note:")` on all four paths.

### 4. Security — see LOW 1
`remove_file`/`upsert_file` are DB-key operations (no FS mutation outside root). `root.join(rel)` reads are DB-derived (trusted indexer). The single gap is the missing containment re-check in `reindex_stale_files` — LOW 1. Confirmed `reindex_stale_files` has exactly one production caller (`try_index`, search.rs:983) passing DB-derived paths; no untrusted-input caller exists.

### 5. Multi-platform neutrality ✓
`mtime_of` (mod.rs:507) and the `fts_page` freshness probe (search.rs:926-931) both use `std::fs::metadata`→`modified`→`duration_since(UNIX_EPOCH)`→`as_millis() as i64` — cross-platform. `Path::join` accepts `/` separators on Windows. `abs.extension()` / `Lang::from_extension` / `std::str::from_utf8` are platform-neutral. Tests use `std::fs::File::set_modified` (stable cross-platform). No Windows-only APIs, no `cfg(windows)`, no drive/UNC assumptions. The `relpath` helper normalizes `\`→`/` for the DB key form. Clean.

### 6. Documentation sync ✓ (plus LOW 2)
- `README.md:52` — updated to the inline-reindex behavior ("at most 8 stale files are re-indexed… 'reindexed N stale file(s)' note… larger staleness or a running index pass serves the authoritative walk").
- `PLAN.md:958-961` — updated ("repairs a small staleness inline (≤ 8 stale files…)… serves the walk with a staleness note only above that cap or while an index pass runs").
- Module/item doc comments — `reindex_stale_files`, `STALE_REINDEX_CAP`, `FtsPage`, `fts_page`, `try_index`, `FtsOutcome::Stale`, `IndexHits.reindexed`, `push_note` all carry doc comments describing the new behavior; the FTS-block intro comments at both call sites (search.rs:1353-1364, search_read.rs:261-269) updated.
- `search`/`search_read` `description()` strings (search.rs:1197-1209, search_read.rs:112-121) never described staleness, so nothing stale to update — confirmed by reading them.
- LOW 2: the historical F10 SPEC record still describes warn-only; a DECISION memory should be written at finish.

### 7. Warning-free build + regression tests
**Build:** I am a read-only reviewer with no `shell` tool, so I could not execute `cargo test`. I inspected for warning sources under `#![deny(warnings)]`: every new symbol is used (`reindex_stale_files` — called from `try_index`; `STALE_REINDEX_CAP` — used in `try_index` + tests; `FtsPage` fields — all read in `try_index`; `IndexHits.reindexed` — read at both call sites; `push_note` — used in both tools; `touched_source`/`pruned`/`refreshed` — all consumed). No new imports were added (mod.rs reuses `DefaultHasher`/`Path`/`Lang`/`FileData`/`mtime_of`; search.rs uses fully-qualified `std::collections::HashSet`; search_read.rs adds none). No `#[allow]`. No obvious dead code. **Recommend the main agent run `cargo test` (root + src-tauri, PowerShell, `$LASTEXITCODE` unpiped) and confirm green before `finish`.**

**Regression coverage** (all behavior changes covered):
- Single "note:" prefix — `stale_index_reindexes_and_serves_fresh_index_results` (`!contains("note: note:")` + `starts_with("note: ")`), `stale_above_the_reindex_cap_walks`, `stale_while_an_index_pass_runs_walks`, `stale_index_reindexes_and_reads_fresh_content` (search_read).
- Reindex → fresh-index serve — `stale_index_reindexes_and_serves_fresh_index_results` ("reindexed 1 stale file(s)", "engine: index", "new bullet (fresh)"); `stale_index_reindexes_and_reads_fresh_content` ("reindexed 1 stale file(s)", "engine: index", "fresh_marker"); `stale_file_beyond_the_display_cap_still_surfaces` ("reindexed 1 stale file(s)", "engine: index", "101 matches in 101 files").
- Walk fallback above cap — `stale_above_the_reindex_cap_walks` ("engine: walk", "index stale for 9 file(s)", fresh "needle v2" + "needle steady").
- Mid-pass walk — `stale_while_an_index_pass_runs_walks` ("engine: walk", "index stale for 1 file(s)", `!contains("reindexed")`, `graph.is_indexing()` still true afterward).
- CodeGraph unit tests — `reindex_stale_files_refreshes_the_given_files` (stored mtime catches up, FTS serves new content, untouched files unchanged), `reindex_stale_files_prunes_vanished_files` (rows gone, content no longer served), `reindex_stale_files_busy_returns_zero_and_leaves_the_flag` (Ok(0), flag untouched, then success clears the flag — covers the plan's intended 4th "clears on return" assertion).

## Recommendation
Fix LOW 1 (add the containment guard — a few lines) and LOW 2 (write the DECISION memory at finish), then run `cargo test` (root + src-tauri) to confirm a warning-free green build, commit, and finish.
