+++
title = "git commit respects existing staging (no blanket add -A)"
created = "2026-08-26"
+++

DECISION (2026-09-08): the `git` tool's `commit` subcommand now respects existing staging. Previously it ran `git add -A` unconditionally before `git commit -m`, which silently overrode any deliberate selective `git add <files>` (via `shell`) and swept in stray untracked files — causing real commit-hygiene bugs (stray cross-branch `.coding/` files landed in the wrong commit). Fix: check `git diff --cached --quiet` first; auto-stage with `git add -A` only when nothing is already staged (backward-compatible common case); when something IS staged, commit only what's staged. Shipped on wt/git-commit-respect-staging (commit 5fe564d, review PASS). Regression test: commit_respects_existing_staging. The shell-for-selective-commits workaround is no longer necessary.
