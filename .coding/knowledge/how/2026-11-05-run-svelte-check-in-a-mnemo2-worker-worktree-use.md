+++
title = "run svelte-check in a Mnemo2 worker worktree — use the main checkout's shim, npx cannot resolve it"
created = "2026-11-05"
+++

Running `npx svelte-check` in a fresh Mnemo2 worker worktree fails: the worktree's node_modules lacks svelte-check (partial install), npx falls back to the registry which times out (ETIMEDOUT), and the npm cache does not have it either (ENOTCACHED). Working invocation, verified 2026-11-05: run the MAIN checkout's shim with the worktree's mnemo2 as cwd — `& C:\mnemo\mnemo2\node_modules\.bin\svelte-check.cmd` from `<worktree>\mnemo2` in PowerShell. It prints "Loading svelte-check in workspace: <worktree>\mnemo2", resolves the project's own node_modules for imports, and reports 0 errors/0 warnings on a clean tree. Read $LASTEXITCODE after it (never a piped exit code). `npx vitest run src/components src/lib` works normally in the worktree (vitest IS installed locally).
