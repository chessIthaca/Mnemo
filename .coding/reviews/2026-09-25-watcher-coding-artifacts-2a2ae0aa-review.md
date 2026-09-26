## Verdict: FINDINGS (0 high, 2 low)

PASS on every requested verification point (pure-refactor claim, loop safety, regression-test genuineness, platform neutrality, standing checks) — with two LOW bookkeeping/hygiene findings to disposition before commit: a stale duplicate plan artifact (7b5b5244.md + its live PLAN memory record) that contradicts the executed plan, and a documented residual on `.worktrees/**` (new watcher churn during run-all, no loop, index-side quirk pre-existing).

## Scope reviewed

ALL uncommitted changes on `wt/mnemo` (HEAD d8152f7, tree = 5 modified + 3 untracked), per `git diff HEAD`:
- `src/tool/agent/search.rs` — `is_coding_data_file` extraction (pure refactor of `should_search`).
- `src/codegraph/watcher.rs` — `is_indexable_path` narrowing, module/fn docs, re-pinned + new tests.
- `docs/FEATURES.md` — coverage-contract bullet.
- `.coding/backlog.jsonl` (9201704f pending→done note, prior plan's landing), the superseded BUG knowledge record (+`status = "superseded"`), its successor `2027-01-11-content-index-staleness-root-causes-coding-never.md`, `.coding/plans/2a2ae0aa.md`, `.coding/plans/7b5b5244.md`.

Cannot run `cargo test` (no shell in my allow-list); parent's evidence (2715/0/5 + 19 integration + 1 doc-test, exit=0, `#![deny(warnings)]` at both crate roots) is consistent with my static verification throughout.
## 1. Pure-refactor claim for `should_search` — VERIFIED ✓

The helper (`src/tool/agent/search.rs:92-109`) is semantically identical to the inlined block it replaced:

- **Same gate**: `rel.components().next() != Some(".coding") → false`. The old inline block had the identical first-component gate on the *same* `rel` (the `strip_prefix(root)` at :122-125 is unchanged and runs *before* the block, so any absolute path whose `.coding` component is not the first *root-relative* component — or any path outside `root` — never reached the block in either version; out-of-root paths fail closed at :124).
- **Same list**: `DATA_FILES` is byte-identical (8 names, same order) — codegraph.db + -wal/-shm/-journal, memory.db + -wal/-shm/-journal.
- **The `path.file_name()` → `rel.file_name()` swap is equivalent for every path that reaches it**: `strip_prefix(root)` succeeded, so `rel` is the tail of `path` — its last component equals `path`'s last component whenever `rel` is non-empty. When `rel` is empty (`path == root`), `rel.file_name()` is `None` (new: not in DATA_FILES → not excluded) and the old code's gate (`None != Some(".coding")`) skipped the block identically. `Path::file_name()` ignores trailing separators on both sides. Trailing `..` edge cases don't normalize in `components()` in either version — identical behavior.
- **Nested cases**: `root/.coding/sub/codegraph.db` → first component `.coding` ✓ + file name ∈ DATA_FILES → excluded, identically in old/new (and excluded on the watcher side too — the two sides agree). `root/x/.coding/codegraph.db` (a `.coding` that is not the first component) → gate fails → searchable, identically old/new on the index side — and now consistently watched on the watcher side (pre-fix the blanket per-component rule rejected it *while the index covered it* — the very asymmetry this plan fixes).

No path previously excluded is now indexed, and none previously indexed is now excluded. The refactor is pure.

**Trigger-set-equals-coverage by construction**: confirmed — `CodeGraph::index_inner` (mod.rs:390) walks via `walk_searchable` (walk.rs:154-159), which delegates its per-file acceptance to `should_search` (verified via the call graph). The content index's coverage set *is* `should_search`; the watcher's trigger set is `!is_ignored_component` + `!is_coding_data_file` — the same predicate pair.

## 2. Loop safety — VERIFIED ✓ (one LOW residual on `.worktrees`, see finding 1)

What an index pass writes under the main `.coding/`:
- `Store::open` (store.rs:142-152) opens exactly `project.codegraph_db` = `<coding_dir>/codegraph.db` (project/mod.rs:65) — one file. The schema pins `PRAGMA journal_mode=WAL` (codegraph/schema.rs:36), so the sidecars are `-wal`/`-shm`; the rollback-journal name `-journal` is additionally covered for any transitional mode. All names are in DATA_FILES → excluded from watching → **no self-trigger**. FTS content rows live in the same DB (cg_content), and `VACUUM` maintenance ops (memory/maintenance.rs) target the memory DB — likewise DATA_FILES.
- **No SQLite `ATTACH` anywhere in src/** (the only "attach" hits are UI/console symbol names) → no multi-database super-journal files (`-mj*`) are possible; single-DB connections can't produce them. No temp-file usage in the codegraph store.
- **The `VACUUM INTO` snapshot** (mod.rs:190-199, `snapshot_db`): its only production caller is `spawn_run_all_agent` (src-tauri/src/ipc/spawn.rs:146-177), dest `<worktree>/.coding/codegraph.db` — under `.worktrees/runall-*/`, **not** the main `.coding/`. It is a one-shot write at worktree spawn; the main watcher now fires one debounced main-tree pass on it, and main passes never write worktree paths → no cycle. (Its other caller is a unit test with a tempdir dest.)
- Churn claims re-verified: `instance.json` is write-once at launch (instance_marker.rs:47-55 — last-writer-wins, no heartbeat, no periodic refresh); `stack.json` is written only on plan-stack transitions (workflow/mod.rs `persist_stack`, with the explicit "unchanged → no rewrite" guard at :843); backlog.jsonl writes are backlog-tool mutations — the intended watched artifact. All debounced into one pass per burst, all content-hash-gated (mtime fast path in store.rs:175-180).

The narrowed exclusion is loop-free for the main tree: an index pass writes only the DATA_FILES family, and those files cannot set the dirty flag (`event_is_indexable` → `is_indexable_path` → `is_coding_data_file` → false).

## 3. Regression test is genuine — VERIFIED ✓

`watcher_reindexes_on_new_coding_artifact` (watcher.rs:451-490):
- **Fails pre-fix for the right reason**: the blanket rule (`|| name == ".coding"`, pre-fix :61) rejected `.coding/knowledge/live-artifact.md`, so the Create event never set `dirty`, no pass ran, the marker never reached cg_content, all 200 polls missed, the assert failed. The re-pinned `is_indexable_path_accepts_any_file_and_rejects_ignored` failed pre-fix on the inverted stack.json case and the new TRUE assertions. That is exactly the parent's observed pre-fix state (exit=101, those two tests).
- **Exercises the changed path end-to-end, not vacuously**: a real `notify::RecommendedWatcher` over a real tempdir; the artifact is written *after* the initial `index(None)` and after `GraphWatcher::spawn`, so a `search_content` hit is reachable only via event → `is_change_kind` → `event_is_indexable` → `is_indexable_path` (the changed function) → `debounce_loop` (100 ms) → `spawn_blocking(index)` → `walk_searchable`/`should_search` admits the .md (content-only rows per mod.rs:385-389) → FTS row with `codingartifactmarker`. The in-memory DB means the pass itself writes no files — no in-test self-trigger.
- **House poll pattern confirmed**: 200 × 50 ms = 10 s, matching both existing watcher tests verbatim (the bound was doubled as FSEvents-latency insurance in plan 5cff52cf); the macOS symlink case rides the untouched canonicalize fallback (pinned by the `cfg(unix)` test). `#[tokio::test]` + `tokio::time::sleep` mirrors `watcher_reindexes_on_new_source_file`.

## 4. Platform neutrality — VERIFIED ✓

No `cfg` gates added, no Windows-only APIs/paths/shell syntax; `components()`/`file_name()`/`to_string_lossy()` are portable; the tests use `tempfile` + `std::fs` (the established idiom — the same pattern as the two prior watcher tests, which are the macOS-CI-burned survivors). The new test keeps the doubled poll bound, so it inherits the FSEvents-latency insurance. The module doc's rewritten claims (`is_ignored_component` + shared store predicate) are accurate — notably it *removes* the previously inaccurate "extension gating" claim (the watcher never had an extension gate). The omitted 1 MB guard remains pre-existing, documented, and harmless (a >1 MB event triggers a cheap no-op pass).

## 5. Standing project checks — VERIFIED ✓ (one LOW finding on plan artifacts)

- **Docs sync**: docs/FEATURES.md bullet updated and accurate (trigger set matches coverage; only SQLite stores excluded). README.md carries no `.coding`/watcher claim (its single "coding" hit is the tagline). PLAN.md's `.coding` mentions are sandbox/plan/backlog mechanics — none stale. agent.md has no watcher mention. Module + fn doc comments rewritten correctly.
- **File-tools-first**: no shell-based file mutation anywhere in the diff (test `std::fs` writes are the established test idiom, exempt).
- **`.coding` artifacts**: the supersede pair lands whole in the same tree (old record +`status = "superseded"`, successor with `supersedes` pointer); the successor BUG record is pointer-first, names the regression test, cites the branch without claiming main-merged; the backlog 9201704f note is consistent with the prior plan's landing (06276aa + round-2 review).
## Findings

### LOW-1 — `.worktrees/**` DB writes now trigger main-tree passes (new churn during run-all, no loop; index-side quirk pre-existing)

Pre-fix, `is_indexable_path`'s blanket per-component rule rejected ANY path with a `.coding` component — including `.worktrees/runall-<item>/.coding/codegraph.db` (the worktree's own graph DB, written continuously by the worktree's index passes and seeded once by `snapshot_db`'s `VACUUM INTO`). Post-fix, `is_coding_data_file` only matches a FIRST-component `.coding`, so those writes are now watched by the MAIN tree's watcher (`.worktrees` is not in `IGNORED_DIRS`, and the worktrees live under the main root per the branch policy).

Assessment: **not a loop** — a main-tree pass never writes anything under `.worktrees`, so the cycle cannot close; the churn is bounded (debounced, one coalesced pass per burst) and only manifests during parallel run-all. It is also *consistent with the pre-existing index side*: `should_search` always used the first-component gate, so the main content index has always "covered" `.worktrees/**` (including worktree SQLite files under 1 MB) — the fix makes the trigger set EQUAL to that coverage, which is the plan's stated invariant. The blanket watcher rule was incidentally suppressing the cost of a pre-existing index-coverage quirk.

Disposition options (fix every finding or justify skipping in writing): fixing inline would change index coverage, which the plan explicitly forbids ("no index-coverage change") — so the right vehicle is a small follow-up backlog item: add `.worktrees` to `IGNORED_DIRS` in `src/tool/agent/search.rs`, which fixes BOTH sides at once (stops the main index ingesting duplicate worktree files AND stops worktree DB writes triggering main passes) and needs the corresponding unit-test re-pin. Justified skip for THIS diff if the follow-up is queued.

### LOW-2 — Stale duplicate plan artifact committed alongside the executed plan (bookkeeping hygiene)

The untracked tree carries TWO plan documents for the same work: `.coding/plans/2a2ae0aa.md` (kind=implementation, all steps `[x]` — the executed plan, the one the successor BUG record cites) and `.coding/plans/7b5b5244.md` (the abandoned/first-cut **bug_fixing** variant — same goal, same anchors, all steps `[ ]`, no superseded/abandoned marker). Memory likewise holds TWO live PLAN records with the identical title (ids 702a0358 and 9dea4744). If both land in the commit, the git-mergeable side-car carries contradictory plan state: one document says the work is done, a second says it has not started (a future session or merge could act on the stale one).

Disposition: before committing, do not land `7b5b5244.md` as a live plan — either drop the stale file from the commit and supersede its PLAN memory record (point it at 2a2ae0aa with a one-line "re-typed bug_fixing→implementation" rationale; memory_supersede, not delete), or add an explicit superseded/abandoned marker to the file mirroring the knowledge-record convention. The same hygiene rule the previous review applied to supersede pairs ("a supersede pair must land whole") argues against landing a contradictory pair.

## Test evidence

`cargo test` could not be run from this reviewer session (no shell in the allow-list). Parent's evidence accepted as stated and found consistent with the code by trace: 2715 passed / 0 failed / 5 ignored, plus 19 integration + 1 doc-test, exit=0, with `#![deny(warnings)]` at both crate roots proving warning-free. The two re-pinned/new tests trace green against the post-fix predicate; the pre-fix failure mode of both is confirmed by reasoning over the pre-fix blanket rule.

## Summary

The core change is correct and the fix's own invariant (trigger set == coverage set, by construction via one shared predicate) holds: `walk_searchable` → `should_search` is the index's coverage, `is_indexable_path`'s `under_base` is the watcher's trigger set, and both now share `is_ignored_component` + `is_coding_data_file` with no other rules. The pure-refactor claim, the loop-safety boundary (DB family only, WAL sidecars covered, no ATTACH, snapshot dest outside the main `.coding/`), the regression test's genuineness (fail-first mechanism + end-to-end event→pass→content-row path + house poll bound), and platform neutrality all check out. Two LOW findings need disposition before commit: the `.worktrees` churn residual (recommend a queued follow-up rather than an inline coverage change) and the stale duplicate plan artifact (drop/supersede 7b5b5244 before landing).