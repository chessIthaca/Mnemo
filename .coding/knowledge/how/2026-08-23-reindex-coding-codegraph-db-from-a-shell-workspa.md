+++
title = "Reindex .coding/codegraph.db from a shell (workspace gotcha + recipe)"
created = "2026-08-23"
+++

HOW: Refresh .coding/codegraph.db from a shell when the app can't (e.g. stale index so finish can't resolve a regression-test symbol).
1. GOTCHA (2026-08): any Cargo.toml created under the repo root is auto-adopted by the root workspace → "current package believes it's in a workspace when it's not". Fix: empty `[workspace]` table in the helper crate's own manifest (self-contained; root workspace.exclude also works).
2. Working recipe (tmp_reindex, verified API): helper crate with `mnemo = { path = ".." }`, main calls `CodeGraph::open(root, root/.coding/codegraph.db).index(None)` → prints scanned/reindexed/symbols (mod.rs:100 open, :188 index — `index(None)` = no progress callback).
3. CLEANER next time: `cargo run --example reindex` — examples are crate-internal, no nested manifest, no workspace error. Or the app's reindex IPC (Graph tab refresh) if the app runs.
4. Cleanup: helper dirs are untracked AND unignored AND their own files get indexed into the graph (walk covers every searchable file) — delete after use.
5. Exit-code masking proven live: the failed cargo run reported exit=0; only stderr told the truth. Trust `$LASTEXITCODE` unpiped + cargo's `error:`/`test result:` lines (agent.md rule).
