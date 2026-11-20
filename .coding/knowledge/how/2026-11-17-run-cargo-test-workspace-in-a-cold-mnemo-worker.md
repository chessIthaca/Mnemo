+++
title = "run `cargo test --workspace` in a cold Mnemo worker checkout"
created = "2026-11-17"
evidence = "Session 2026-?? on wt/agent-findingC (plan 3c2ec71c step 1, commit 70420f27): first `cargo test --workspace` timed out at 300s; after `--no-run` warm-up the exact command passed (exit 0, 3547+845+102 tests); the stale-claim test failed once in the full run and passed alone (1 passed)."
+++

A cold Mnemo worker worktree (.worktrees/pool-<n> with no target/) cannot finish `cargo test --workspace` in one shell call: the shell tool kills commands at 300s, and the first run compiles all workspace test targets. Fix: copy C:\Mnemo\target\mnemo2-dist into <checkout>/target, run `npm ci --no-audit --no-fund` at the root, then warm the build with repeated `cargo test --workspace --no-run` calls (each ≤300s, progress resumes) until it reports "Finished" in a few seconds; only then run the exact check command, which then completes (compile warm, ~30-80s of execution). Also: `app::worktrees::tests::a_stale_claim_is_taken_over_and_removed` (src/app/worktrees.rs) flakes under full-suite parallelism — it can panic with pool-1 "has a live claim; skipped" while the same test passes in isolation (`cargo test -p mnemo --lib app::worktrees::tests::a_stale_claim_is_taken_over_and_removed`), so re-run the workspace suite rather than hunting the pool code.
