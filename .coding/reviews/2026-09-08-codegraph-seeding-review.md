## Verdict: FINDINGS (0 high, 5 low)

The seeding change is correct and well-designed: `VACUUM INTO` on a read-only connection is valid SQLite, platform-neutral via the bundled amalgamation, and the content-hash re-check makes seeding safe by construction — the regression test proves the re-parse-exactly-the-changed-file semantics. All five findings are LOW polish: an imprecise torn-copy rationale in the doc comment, a duplicated DB-path derivation, a probabilistic (not guaranteed) mtime fast-path assumption, a test that doesn't pin the live-WAL property, and a silently swallowed seed failure.


## Scope & method

Reviewed `git diff HEAD` on wt/agenticcoding — src/codegraph/mod.rs (+85: `snapshot_db` + regression test), src-tauri/src/ipc/spawn.rs (+55/−11: seeding in `spawn_run_all_agent` + timing log + doc comments), and the `.coding/backlog.jsonl` status flip (app bookkeeping, expected). Verified against the surrounding code, not just the diff: store.rs/schema.rs (WAL pragmas, transactional `upsert_file`, `rebuild_edges`), mod.rs (`index` mtime fast path, `mtime_of`), factory.rs `codegraph_handle`, main.rs:1497 (main graph open), project/mod.rs (path derivation), run_all.rs `fill_spawned_window`/`dispatch_spawned_item` (worktree lifecycle), README/PLAN, and the SQLite `VACUUM INTO` documentation. The reviewer surface has no shell — every claim below is verified by reading the code; the reported green `cargo test --workspace` is consistent with the code as read.

## Design decisions verified sound

1. **VACUUM INTO vs file copy — sound.** SQLite docs (lang_vacuum.html): "VACUUM INTO … is transactional in the sense that the generated output database is a consistent snapshot of the original database", and "VACUUM (but not VACUUM INTO) is a write operation" — so the `SQLITE_OPEN_READ_ONLY` connection (mod.rs:136) is valid for it. rusqlite is `bundled` (Cargo.toml:65): the same SQLite amalgamation compiles on macOS and Windows, so the Windows-passing test carries over with no system-SQLite variance. The dest-must-not-exist rule is unreachable in practice: worktrees are freshly provisioned and never reused (run_all.rs:3635-3638 — provisioning fails when a previous branch/worktree lingers, "surfaced, never silently reused"), and codegraph.db is gitignored (README:134), so a fresh worktree never carries one.
2. **Best-effort silent fallback — appropriate for a cache.** Never worse than cold; the timing line (spawn.rs:184-196) surfaces seeded|cold with the full delta. One observability nit → LOW-5.
3. **Partial seed + concurrent lanes — fine.** The main DB is WAL with busy_timeout=5000 (schema.rs:34-41); each lane's read-only VACUUM INTO is a non-blocking WAL reader, and a main index still running at dispatch time just yields a partial seed that the per-file hash check heals (files with rows skip, files without rows parse).
4. **Main-DB path derivation — correct today** (`g.root().join(".coding").join("codegraph.db")` matches the live main graph's `project.codegraph_db`, main.rs:1497 + project/mod.rs:65); the duplication itself → LOW-2.
5. **Seeded-graph correctness.** Persisted `cg_refs` + `rebuild_edges` on `reindexed > 0` (mod.rs:404-406) re-derives cross-file edges for the worktree's symbol set; `prune_missing` drops main-only files; FTS content rows are hash-checked like everything else. The lane's graph answers for the worktree's files, never the main tree's.
6. **eprintln! timing log** matches the module's convention (mod.rs:343, store.rs:113-116) and every IndexStats field it prints exists (mod.rs:57-72).


## Findings

### LOW-1: snapshot_db's torn-copy rationale cites a tear the store's transactional invariant forbids

- **Location:** src/codegraph/mod.rs:119-128 (doc comment on `snapshot_db`).
- **Symptom:** The comment justifies VACUUM INTO over a plain copy with "a `cg_files` row committed while its `cg_symbols` rows are still in the WAL … a file whose row survived but whose symbols did not would be skipped with missing symbols." But `Store::upsert_file` replaces one file's `cg_files` + `cg_symbols` + refs + FTS rows in a SINGLE transaction (store.rs:10-12) — that split cannot occur. The realistic non-self-healing tear is a different one: a copy that includes the upsert commits but misses the trailing `rebuild_edges` commit serves new symbols with stale cross-file edges, and when `reindexed == 0 && pruned == 0` the lane's own rebuild never fires (mod.rs:404-406), so the stale edges persist. Missed whole-file commits, by contrast, ARE self-healing (fresh parse).
- **Why it matters:** The conclusion (use VACUUM INTO) is right, but a future maintainer reading the wrong mechanism could "simplify" back to a file copy believing only the impossible tear matters, or guard the wrong invariant.
- **Fix direction:** Reword to the accurate hazards (missed `rebuild_edges` commit → stale edges, not self-healing; db/`-wal`/`-shm` read at different moments → incoherent trio), or simply cite VACUUM INTO's documented consistent-snapshot property.

### LOW-2: the main-DB path convention is duplicated instead of derived

- **Location:** src-tauri/src/ipc/spawn.rs:169-171.
- **Symptom:** `g.root().join(".coding").join("codegraph.db")` re-hard-codes the convention that project/mod.rs:59-74 owns (`Project::from_root(...).codegraph_db`, the path the live main graph is opened with at main.rs:1497).
- **Why it matters:** If the DB path convention ever changes in `Project`, spawn.rs silently seeds from a non-existent file — the READ_ONLY open fails, seeding no-ops, and every lane falls back to the cold parse with no error anywhere (only the timing line would say "cold"). A silent perf regression with no tripwire.
- **Fix direction:** `mnemo::project::Project::from_root(g.root().to_path_buf()).codegraph_db` — one source of truth (the type is already pub and used from src-tauri).

### LOW-3: the seeded pass's correctness leans on mtimes differing — probabilistic, not guaranteed

- **Location:** src/codegraph/mod.rs:316-339 (mtime fast path) as reached from src-tauri/src/ipc/spawn.rs:178-196.
- **Symptom:** The fast path skips the hash check when `stored_mtime == disk_mtime` (mod.rs:332). Within one tree that is sound (a content write always bumps mtime). Cross-tree it is not: the stored mtime is the MAIN tree's last write of the file; the disk mtime is the worktree CHECKOUT time — unrelated clocks. If a branch-diff file's checkout lands in the same millisecond as its last main-tree write (`mtime_of` is ms-resolution, mod.rs:727-740 — e.g. the main agent saving file X exactly as a lane's worktree checks X out), the file is skipped without a hash check and the lane's graph serves the main tree's uncommitted symbols for X for the lane's lifetime.
- **Why it matters:** Remote (needs the same-ms coincidence AND uncommitted diffs on that exact file) but it is a real correctness edge introduced by seeding — before this change a cold pass had no stored rows to wrongly trust. The test's own comment ("fresh mtimes miss the mtime fast path") documents the reliance without making it deterministic.
- **Fix direction:** Make the seeded sweep deterministic — e.g. a flag on the index pass that bypasses the mtime fast path when seeded, so every file is hash-checked exactly once — or document the residual explicitly in the doc comment.

### LOW-4: the regression test doesn't pin the property that motivated VACUUM INTO

- **Location:** src/codegraph/mod.rs:1270-1322 (`snapshot_db_seeds_and_reindexes_only_changed_files`).
- **Symptom:** Tree A's `CodeGraph` handle is dropped at the end of its block BEFORE `snapshot_db` runs — the closing connection checkpoints and removes the `-wal`, so the test snapshots a quiescent DB. A naive `fs::copy(db_a, db_b)` implementation would pass this test identically; the live-WAL consistency property (the actual reason VACUUM INTO was chosen over a copy) is untested.
- **Why it matters:** A future refactor of `snapshot_db` to a plain copy would keep the suite green while reintroducing the live-DB tear hazard the doc comment warns about.
- **Fix direction:** Keep tree A's graph handle open across the `snapshot_db` call (its committed rows then live in the un-checkpointed WAL — a small DB never hits the 1000-page auto-checkpoint), so a db-only copy yields an empty DB and the `files_reindexed == 1` assertion fails, while VACUUM INTO (reading through the WAL) still passes.

### LOW-5: the seed failure is swallowed without a trace

- **Location:** src-tauri/src/ipc/spawn.rs:174-177.
- **Symptom:** `snapshot_db(src, &db).is_ok()` discards the error. A lane that silently degrades to cold (locked DB, missing file, dest-exists) leaves only the word "cold" in the timing line — no reason — unlike every other best-effort failure in the module, which gets an eprintln (mod.rs:343, store.rs:113-116), and unlike the index failure, which IS logged (spawn.rs:195).
- **Why it matters:** When seeding stops working in the field (e.g. after a path-convention drift, LOW-2), there is nothing to diagnose from.
- **Fix direction:** One line on failure — `eprintln!("codegraph: worktree seed failed (cold pass): {e}")` — matching the module's convention; the fallback itself stays silent-by-design.


## Constitution checks

- **Documentation sync — clean.** The updated doc comments (`WorktreeBinding.codegraph`, `spawn_run_all_agent`, `snapshot_db`) match the new behavior. README:81's "a fresh code-graph index over the worktree tree" remains accurate at the feature level — each lane still gets its own newly-created graph over its own tree (seeding is an internal perf detail; a new DB file is still created). PLAN.md's worktree rows are branch-topology only and make no cold-index claim. No README/PLAN update is required for this change.
- **Multi-platform neutrality — clean.** rusqlite `bundled` (Cargo.toml:65) compiles the same SQLite amalgamation on macOS and Windows; only Path/PathBuf joins, no `cfg(windows)`, no shell or path-syntax assumptions. The `to_string_lossy` dest path (mod.rs:138) is only lossy for non-UTF8-representable paths, which the app-derived worktree paths (project root + `.worktrees/runall-<hex>`) cannot produce; if ever hit, the failure mode is benign (a mojibake side file + cold pass).
- **Regression test** meets the backlog item's acceptance criteria (a seeded DB with one changed file re-parses exactly that file; results identical to cold). LOW-4 notes the strengthening opportunity around the live-WAL property.
- **Public API hygiene:** `snapshot_db` is `pub` with a doc comment (constitution requirement); imports added are minimal and used.

## Bottom line

Ship it. The core mechanism — a `VACUUM INTO` consistent snapshot of the live WAL-mode main DB, then an index pass whose content-hash check re-parses only branch-diff files — is correct, never worse than cold, and platform-neutral; the regression test proves the acceptance semantics and the timing log surfaces the win. The five LOWs are precision/robustness polish, none blocking: LOW-2 and LOW-5 are one-liners worth fixing now; LOW-1's reword and LOW-4's test strengthening fit a follow-up; for LOW-3, either make the seeded pass hash-only (deterministic) or document the residual in the doc comment.
