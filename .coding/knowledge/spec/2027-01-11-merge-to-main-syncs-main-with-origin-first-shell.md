+++
title = "merge_to_main syncs main with origin first + shell/git-tool split"
created = "2027-01-11"
+++

The merge_to_main flow (skill + Git tab override prompt) now syncs main with origin BEFORE the branch merge, and splits tools by approval-gate semantics:

SEQUENCE (all copies agree: `.coding/skills/merge_to_main.toml`, `frontend/src/components/views/GitView.tsx::mergePrompt`, agent.md, PLAN.md:320, docs/FEATURES.md:34):
1. commit all work incl. .coding/ on the branch (NEVER stash); 2. `git checkout main` → `git fetch origin` + `git pull --no-rebase` VIA SHELL (the git tool has no fetch/pull subcommand) — conflicts: UNION + add/commit; 3. `git merge --no-ff <branch>` VIA THE GIT TOOL (never shell); 4. verify `npm run build` AND `cd src-tauri && cargo build`; 5. `git branch -d <branch>`; 6. supersede branch-status memories; 7. skill_end (+ optional approval-gated push).

WHY THE TOOL SPLIT MATTERS: `GitTool::never_auto_for` (src/tool/agent/git.rs:516) is subcommand-aware over the runtime `core_operations` list (defaults merge/push) and is consumed at src/agent/dispatch.rs:339 to force the approval prompt even in Autonomous mode. `ShellTool` does NOT override `never_auto_for` (src/tool/agent/shell.rs — name/category/schema/safety/execute only), so a shell-invoked `git merge`/`push` runs UNPROMPTED. That is why only the sync may use shell. Backlog candidate (not queued): a command-aware ShellTool guard.

ALSO LANDED: `.coding/instance.json` is gitignored beside `.coding/plans/stack.json` (per-instance {pid, started_at}, last-writer-wins — the pull step would otherwise conflict on pid churn); guards pin fetch→pull→merge ordering in `src/skill/mod.rs::shipped_skill_files_parse_and_merge_to_main_cleans_up_memories` and in the new `frontend/src/components/views/GitView.test.ts` (registered in vitest.config.ts).

Motivation: on 2026-09-13 a stale main surfaced as a rejected push only AFTER the branch landed, costing a separate catch-up session. Commit 06fdf7a on wt/mnemo; reviews 2026-09-14-merge-to-main-remote-sync-review.md (findings) + -round2.md (PASS).
