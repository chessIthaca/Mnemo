+++
title = "Mnemo2's Git tab moves main by safe fast-forward only and never pushes"
created = "2026-11-02"
+++

The lead's Workbench has a Git tab (GitTab.svelte, lib/git.ts, src-tauri/src/mnemo2/git_view.rs): current branch, branches with ahead/behind main, the latest commits, and where main is checked out. "Move main here" asks for confirmation (naming the commit count), refuses while main is checked out in any worktree (saying where, and whether it has uncommitted changes), requires `git merge-base --is-ancestor main <branch>`, and moves the ref with `git update-ref` using the old value as compare-and-swap; it never pushes or rewrites history. Workbench tabs are now Browser, Diff, Plan, Files, Trace, Git (all in WORKBENCH_TABS).
