## Verdict: FINDINGS (0 high, 3 low)

Review of the uncommitted changes on wt/agenticcoding (src/codegraph/mod.rs +29, src/tool/agent/codegraph.rs +160/−4, .coding/backlog.jsonl run-all bookkeeping) implementing plan 55163f1e / backlog 95f21af0 — graph_search stale-index miss, inline reindex-and-reserve (F10 pattern for the symbol index).

The core implementation is sound: the spawn_blocking flow mirrors run_query's error shaping exactly, the reindexed/unrepaired_stale state machine is correct and mutually exclusive by construction, the `.ok().filter(|n| *n > 0)` handling matches the documented Ok(0)-busy/Err semantics, the mtime comparison is millis-vs-millis (consistent with `mtime_of` and the stored values), the no-staleness payload is byte-identical to the pre-change shape, and there is no deadlock or lock-ordering hazard. Three low findings below; none block the merge.

### Findings

**LOW 1 — The re-resolve view failure errors the lookup, contradicting the stated best-effort invariant** — src/tool/agent/codegraph.rs:330

```rust
Some(n) => {
    let fresh = graph.view().map_err(|e| e.to_string())?;   // ← propagates
    matches = fresh.resolve(&args.query);
    reindexed = Some(n);
}
```

The comment at :305-307 (and the plan's design decision 6) promises "a lookup is never failed by its own repair" / "all freshness errors fall through to the plain miss — never a ToolResult::error from the repair path". Every other freshness error does fall through — `stale_source_files()` Err is swallowed by `if let Ok` (:316), reindex Err by `.ok()` (:323-326) — but a failure of the SECOND `graph.view()` (after the first view loaded fine and the reindex wrote successfully) propagates via `?` to `Ok(Err(msg))` → `ToolResult::error`. The window is narrow (an SQLite read failing right after a successful write on the same process-local mutex), and surfacing it is defensible under run_query's "a broken DB must surface" philosophy — but as written the code and its invariant disagree. Fix either way: (a) harden — on the fresh-view error keep the (empty) matches, still set `reindexed = Some(n)` (the side effect happened and must be disclosed), and skip the re-resolve; or (b) narrow the comment to state that a view failing to load after a successful reindex surfaces as an error (broken DB). One line either way.

**LOW 2 — The unrepaired-staleness note has zero test coverage** — src/tool/agent/codegraph.rs:336-338 (above-cap branch) and :334 (Ok(0)/Err branch)

The "symbol index may be stale — {n} file(s) on disk are newer than the index" note — a distinct user-visible behavior this change introduces — is never asserted by any test. The F10 sibling pins both of its analogues (search.rs:3223 `stale_above_the_reindex_cap_walks`, :3270 `stale_while_an_index_pass_runs_walks`). Both branches are cheaply testable with the existing fixture: (a) above-cap — write 9 source files, index, bump all mtimes (`set_modified(now+5s)`, the same technique the new regression test uses), query an absent symbol → assert the "may be stale — 9 file(s)" note, no "reindexed", hint still present; (b) busy-pass — `graph.set_indexing(true)` (pub, already exercised by `reindex_stale_files_busy_returns_zero_and_leaves_the_flag`) with one stale file → same note, no reindex. One test (the above-cap one) would suffice to pin the note string and the no-inline-reindex-above-cap behavior.

**LOW 3 — Pre-existing, now load-bearing: stored_mtimes docs say "unix seconds" but the values are unix millis** — src/codegraph/mod.rs:181 and src/codegraph/store.rs:619

`mtime_of` (mod.rs:563-576) returns `as_millis()`, `upsert_file` stores that, and `stored_mtimes` returns it — so the M2 fast path, the F10 sweep, and the new `stale_source_files` all compare millis-to-millis (correct). But both doc comments on `stored_mtimes` say "(unix seconds)". Not introduced by this diff — however, this change makes that doc the entry point for a second dependent sweep: a maintainer "correcting" `mtime_of` to seconds to match the doc would silently break the M2 fast path and both staleness detectors (same-second edits would collide). One-word fix in each doc comment ("unix millis") — ideally riding along with this change since it touches the machinery.

### Correctness verification (checked, no findings)

- **Error shaping:** the new closure's outcome handling (:372-380) is identical in shape to `run_query` (:62-75) — view errors → `ToolResult::error(msg)`, join errors → `"graph query task failed: {e}"`. The initial `graph.view()` error path matches run_query's pre-existing semantics exactly.
- **State machine:** `reindexed` and `unrepaired_stale` are mutually exclusive by construction (the `match repaired` arms at :327-338); note emission checks `reindexed` first (:360-368). A repaired-but-still-missing retry serves note + hint together — intended per the design, and pinned by `vanished_stale_file_degrades_gracefully` (:852-871).
- **`.ok().filter(|n| *n > 0)`:** `Ok(0)` (busy pass, or all-files-skipped) and `Err` both → `None` → unrepaired note; the note text stays accurate in both cases. Partial success (n < stale.len(), some files skipped) reports the true n. ✅
- **Borrow/move:** `graph` (Arc) and `args` moved into the closure; `args.query` borrowed until the final `json!` — compiles clean under `deny(warnings)`.
- **mtime units:** millis-vs-millis — `mtime_of` returns `as_millis()`, `upsert_file` stores it, `stale_source_files` compares like-for-like (see LOW 3 for the doc discrepancy only).
- **Byte-identity:** with no staleness the payload is `{query, count, symbols}` (+`hint` on miss) — same construction and same `to_string_pretty` as before the change. `miss_hint` and the "No symbols matched" steering marker are unchanged; a repaired miss serves symbols (no marker — correct), an unrepaired miss keeps it (correct).
- **Edge cases:** `stale.len() == 8` repairs (`<=`), 9 notes; vanished file → `mtime_of` 0 ≠ stored → flagged → pruned by the reindex (refreshed += 1, disclosed); empty `stored_mtimes` (unindexed graph) → plain miss, no note — no false "may be stale" on a fresh project; near-miss (candidates exist) skips the sweep entirely (by design — the total miss is the staleness signal); non-source content rows are correctly excluded by the `Lang::from_extension` filter (symbols only come from source files).

### Concurrency (checked, no findings)

- **No nested locks:** `stale_source_files` → `stored_mtimes` takes the store lock for one SELECT and releases; the per-file stats run unlocked; `reindex_stale_files` claims the indexing flag atomically (`compare_exchange`, mod.rs:398-406) and takes the store lock per file; `view()` takes it for two SELECTs. Single mutex, never held across a wait. ✅
- **No deadlock:** the inline reindex never waits — it claims or returns `Ok(0)`; the watcher's debounce loop waits on the flag with sleep-polling (watcher.rs:202-204) holding no lock, and its `index()` pass takes the store lock per file. ✅
- **Pre-existing observation (not a finding, unchanged by this diff):** `CodeGraph::index` sets the flag with an unconditional `store` (mod.rs:248), not `compare_exchange`, so the watcher's check-then-index (watcher.rs:202-206) can start a full pass concurrently with a just-claimed inline reindex. Benign — per-file upserts are mutex-serialized and idempotent (same content → same hash → same rows); worst case is duplicate work and an early flag clear. The window has existed since the F10 fix (the search tools call the same `reindex_stale_files`); this change only adds a caller.

### Test quality (checked)

- `stale_symbol_miss_reindexes_and_serves_fresh_results` (:797-834) genuinely pins the fix: without the freshness flow the count is 0 and there is no note — the `count == 1` assert fails. The mtime-bump technique (`fs::write` + `File::set_modified(now+5s)`) is sound and deterministic on Windows (std `set_modified` → SetFileTime with a write handle; +5s clears any filesystem timestamp granularity, incl. FAT's 2s) and matches the search tool's own tests (search.rs:3238-3247). ✅
- `fresh_miss_stays_fast` (:836-850) pins the no-false-positive path: a spurious staleness detection would produce a note and fail the assert. ✅
- `vanished_stale_file_degrades_gracefully` (:852-871) pins the prune + note + hint coexistence. ✅
- Gap: LOW 2 (the unrepaired-staleness note branches).

### Constitution (checked)

- **Documentation sync:** module doc gained the staleness paragraph (:27-35 — accurate, including the brand-new-files residual); `stale_source_files` and `STALE_REINDEX_CAP` are fully doc-commented. README/PLAN.md need no update — README:53's steering contract ("every `graph_*` miss points at `search`") remains true, and the note is documented in the module doc exactly as the F10 sibling's note was (no README change shipped for F10 either). ✅
- **Multi-platform neutrality:** pure `std::fs`/`std::time`; `set_modified` is cross-platform std; forward-slash rel keys joined onto root work on Windows (same pattern as the pre-existing `reindex_stale_files`). No `cfg(windows)`, no platform APIs. ✅
- **Code style:** doc comments on all new items, no `#[allow]`, `deny(warnings)`-green per the parent's test matrix. ✅

### Security (checked)

- `stale_source_files` is stats-only (no reads, no parses). `reindex_stale_files` retains its canonicalize containment guard (out-of-root keys skipped, never read); graph_search feeds it only indexer-written keys from `stored_mtimes`. The tool stays `AutoRun` with a bounded (≤8 files), always-disclosed side effect on a derivable local cache DB. No new attack surface. ✅

### Summary for the parent

Fix LOW 1 (one-line hardening or comment narrowing at codegraph.rs:330), LOW 2 (add one above-cap test pinning the "may be stale" note), LOW 3 (two one-word doc fixes at mod.rs:181 / store.rs:619), then re-run `cargo test` (root crate covers both touched files) and commit. All three are low-severity; the core change is correct and mergeable once they're addressed.
