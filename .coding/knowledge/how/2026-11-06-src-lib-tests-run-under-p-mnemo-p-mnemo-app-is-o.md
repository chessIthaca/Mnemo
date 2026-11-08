+++
title = "src/ lib tests run under -p mnemo; -p mnemo-app is only the src-tauri shell"
created = "2026-11-06"
+++

The root Cargo package `mnemo` owns src/ (memory, indexer, tool, agent, runtime): run its tests with `cargo test -p mnemo --lib <filter>` and lint with `cargo clippy -p mnemo --all-targets -- -D warnings`. `cargo test -p mnemo-app --lib` runs only the src-tauri shell (about 9 tests), so a filter such as `memory::indexer` matches nothing there and a green run proves nothing about src/ edits. When a plan or brief names `-p mnemo-app` for a src/ change, use `-p mnemo` (and run mnemo-app too only as the shell gate). A plan's verification commands must name the package that owns the edited files. Verified 2026-11-06: `cargo test -p mnemo --lib` 3388 passed, `cargo test -p mnemo-app --lib` 9 passed, clippy -D warnings clean for both. From a lead shell, run a worker worktree's tests with `cargo test --manifest-path <worktree>/Cargo.toml -p mnemo --lib ...` instead of cd-ing into it: a shell whose working folder is inside a worktree blocks the merge's worktree cleanup (os error 32).
