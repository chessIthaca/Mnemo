+++
title = "run-all worktree codegraphs seeded via VACUUM INTO snapshot + deterministic index_seeded"
created = "2027-01-07"
+++

Run-all worktree codegraphs (backlog 6c56a5a1, commit 460524b on wt/agenticcoding) are now SEEDED from the main tree's live DB instead of cold-parsing every file. Design decisions:
1. VACUUM INTO snapshot, never a file copy — snapshot_db (src/codegraph/mod.rs:113-144) opens the source read-only, busy_timeout(5s), VACUUM INTO dest. A plain copy of a live WAL DB can miss the trailing rebuild_edges commit → new symbols with stale cross-file edges, NOT self-healing (a seeded pass with reindexed==0 never fires its own edge rebuild); VACUUM INTO is documented to produce a consistent snapshot reading through the WAL.
2. index_seeded (mod.rs:299-308) delegates to index_inner(trust_mtime=false) — cross-tree mtimes are meaningless (fresh checkout), so the seeded pass bypasses the mtime fast path and content-hash-checks every file exactly once, deterministically. Regular index() keeps trust_mtime=true (bit-identical for pre-existing callers).
3. Seeding is best-effort inside spawn_blocking in spawn_run_all_agent (src-tauri/src/ipc/spawn.rs:164-215): seed → CodeGraph::open → index_seeded; any failure eprintln's "codegraph: worktree seed failed (cold pass)" and falls back to a cold index — a degraded lane never blocks dispatch.
4. Main-DB path derived via Project::from_root(g.root()...).codegraph_db — the same single source of truth the live main graph was opened with (src-tauri/src/main.rs:1496-1497).
Regression test snapshot_db_seeds_and_reindexes_only_changed_files (mod.rs:1299-1357) pins both semantics: seeded DB + one changed file → re-parse exactly that file; and the live-WAL property (tree A's graph held open across snapshot_db, so a naive db-only fs::copy would fail). Reviews: round 1 FINDINGS 0H/5L (all fixed), round 2 PASS (.coding/reviews/2026-09-08-codegraph-seeding-review-round2.md).
