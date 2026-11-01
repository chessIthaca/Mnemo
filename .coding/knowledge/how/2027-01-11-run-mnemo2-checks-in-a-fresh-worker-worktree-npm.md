+++
title = "run mnemo2 checks in a fresh worker worktree (npm ci first)"
created = "2027-01-11"
+++

Fresh Mnemo worker worktrees (C:\Mnemo\.worktrees\agent-*) are gitignored-clean: they have NO node_modules, so `npm run check`/`npm test` in mnemo2/ fail with "'svelte-check' is not recognized". Fix before any check run: `npm ci --no-audit --no-fund` at the WORKTREE ROOT (the repo root is the npm workspace root — workspaces: frontend, mnemo2; deps hoist to root node_modules). ~302 packages, ~12s on this machine. The root .npmrc pins registry.npmjs.org and disables audit/fund. PowerShell note: run shell commands unpiped or read $LASTEXITCODE — piped cmdlet output can show stderr noise (NativeCommandError) even on success.
