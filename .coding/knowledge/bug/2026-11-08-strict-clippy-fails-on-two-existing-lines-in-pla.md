+++
title = "Strict clippy fails on two existing lines in plan_board.rs under clippy 1.95"
created = "2026-11-08"
status = "superseded"
+++

Symptom: `cargo clippy --all-targets -- -D warnings` exits 101 on any clean branch, so a worker's run looks like its own failure. Cause: src/app/plan_board.rs has `Some(reason) if reason.is_empty()` (clippy::redundant_guards, suggests `Some("")`) and `!...is_some_and(|files| !files.is_empty())` (clippy::nonminimal_bool, suggests is_none_or). There is no rust-toolchain.toml, so the machine's stable clippy decides. Workaround used in reviews: add `-A clippy::redundant_guards -A clippy::nonminimal_bool`, which means a "clippy clean" report may carry that allowance. Fix status: not fixed yet.
