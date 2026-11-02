+++
title = "Worker shell commands never hang on a background process"
created = "2026-11-02"
+++

Symptom: A worker shell command that leaves a process running (e.g. `npx vite` started with `&`/Start-Process, or any dev server) never returns: the worker hangs for 50-90+ minutes, and the server outlives the worker and blocks its worktree from being removed. · regression test: background_child_holding_the_pipes_does_not_hang_the_call

Full record for plan bfe2b09f (see .coding/plans/bfe2b09f.md for the plan file).

regression test: background_child_holding_the_pipes_does_not_hang_the_call · path .coding/plans/bfe2b09f.md · branch wt/mnemo @ 6bffdcc9 (unmerged — exists only on this branch)
