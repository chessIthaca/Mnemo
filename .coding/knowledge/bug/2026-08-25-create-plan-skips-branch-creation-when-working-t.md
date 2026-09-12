+++
title = "create_plan skips branch creation when working tree has changes outside .coding/ — commit landed on main"
created = "2026-08-25"
+++

Root cause: create_plan was called while the working tree had changes outside .coding/ (stray uncommitted reverts of merged R9/R10 work in src/provider/*.rs). Per create_plan's design, branch creation is SKIPPED when the tree has changes outside .coding/ — so the plan ran on the current branch, which was `main`. The subsequent `git commit` landed directly on main (commit 5c37673), violating the "never commit to main" hard rule.

Fix applied: created `fix/read-files-path-shorthand` branch at the commit, checked out to it, and force-reset main back to f6a4f7b (the previous commit). The commit now lives only on the feature branch.

Lesson: BEFORE calling create_plan, check `git status` for changes outside .coding/. If any exist, either restore them to HEAD first (so create_plan can fork a clean working branch) or explicitly pass a `branch` name to create_plan. Never assume create_plan created a working branch when the tree is dirty — it silently skips branch creation and you end up on whatever branch you're currently on (potentially main).
