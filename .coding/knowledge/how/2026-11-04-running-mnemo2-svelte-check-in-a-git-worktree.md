+++
title = "running Mnemo2 svelte-check in a git worktree"
created = "2026-11-04"
+++

In a Mnemo worktree (`C:\Mnemo\.worktrees\<agent>\mnemo2`), `npm run check` fails with "'svelte-check' is not recognized" because the worktree has no local node_modules — only Vite caches exist there, and packages resolve from the hoisted `C:\Mnemo\node_modules` (workspace root), which holds vitest but NOT svelte-check. svelte-check is installed nested only at `C:\Mnemo\mnemo2\node_modules`. Run it from the worktree's mnemo2 directory: `& C:\Mnemo\mnemo2\node_modules\.bin\svelte-check.cmd --tsconfig ./tsconfig.json` (parses the worktree's 4521 files, exit 0). `npx vitest` works from the worktree because vitest is hoisted to `C:\Mnemo\node_modules`.
