+++
title = "Mnemo2 team tests flake on fixed poll budgets — Completed precedes the terminal transcript refresh"
created = "2026-11-23"
evidence = "plan 0908a4d0 step 1, commit 08c1c997; before 2/30 failures at runtime/tests.rs:2301 under 4x load, after 0/30; cargo test --workspace + clippy pass"
+++

A Mnemo2 worker is marked Completed (and emits Finished) BEFORE its transcript is attached: the pump spawns a task after the terminal event that sleeps 10 ms, then snapshots the session, probes the git tip and saves the ledger (src-tauri/src/mnemo2/team/pump.rs ~:1473-1521). On a loaded machine that refresh lands seconds later, so any test polling the team snapshot for "Completed with a non-empty transcript" under a fixed budget flakes (e.g. runtime/tests.rs team_runs_two_workers_concurrently_without_cross_talk: a 100x20 ms = 2 s budget failed 2/30 under 4x mnemo2::team load, 0/30 after replacing it with a condition wait bounded only by a 120 s hang guard). Same class: fixed per-event recv timeouts while worktree creation/git runs. Fix pattern: HANG_GUARD (120 s) + condition wait that panics naming what it waited for; never widen a budget. Product behaviour is by design (the UI shows the transcript a beat after Completed), so the tests were the defect.
