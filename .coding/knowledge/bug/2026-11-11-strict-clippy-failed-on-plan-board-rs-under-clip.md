+++
title = "Strict clippy failed on plan_board.rs under clippy 1.95 — FIXED 2026-11-11 (b9cbaee4)"
supersedes = "2026-11-08-strict-clippy-fails-on-two-existing-lines-in-pla"
created = "2026-11-11"
evidence = "Lead-run on main b98183a2: cargo clippy --workspace --all-targets exit 0; cargo clippy -p mnemo-app --all-targets --features mnemo2 exit 0."
+++

The two lints in src/app/plan_board.rs (:418 redundant_guards, :1508 nonminimal_bool) that made `cargo clippy --all-targets -- -D warnings` exit 101 on every clean branch are fixed. So are three more (runtime/agent.rs drop_non_drop, optimizer.rs needless_update, memory/tests.rs useless_vec): merge b9cbaee4, plan 3ce7d708, on main via PR #117. A clippy failure on a fresh branch is now the branch's own. Plan 70e12e6d adds .github/workflows/ci.yml so PRs run both clippy commands from then on.
