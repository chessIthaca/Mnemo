+++
title = "land a Dependabot npm PR's exact versions in the mnemo2 workspace (npm rewrites caret specs; jsdom is override-pinned)"
created = "2026-11-23"
evidence = "Release v2.8.0 plan aabd18a3 step 1, branch wt/agent-depsFold commit a18b9022: npm installs first produced @lucide/svelte 1.78.0 / dompurify 3.5.0 / svelte 5.59.1, then restoring the Dependabot branch files + `npm ci` gave the PR's exact versions; `npm ls --workspace mnemo2 --depth 0` reported \"@lucide/svelte@1.77.0, @tauri-apps/api@2.12.5, @tauri-apps/cli@2.12.6, dompurify@3.4.18, jsdom@31.0.0 overridden, svelte@5.59.0, vitest@5.1.1\" and `npx --no-install tauri --version` printed \"tauri-cli 2.12.6\"."
+++

Landing a Dependabot npm PR's exact versions in this repo's workspace is not what `npm install <pkg>@^<ver> -w mnemo2` does: npm resolves the newest satisfying release and rewrites the specifier to it (observed 2026-11-23: `^1.77.0` → `^1.78.0`, `^3.4.18` → `^3.5.0`, `^5.59.0` → `^5.59.1`), so the lock drifts past the PR it was meant to fold in.

Reliable route:
1. Confirm the Dependabot branch's base matches HEAD for the npm files (`git diff --stat HEAD origin/dependabot/<branch>~1 -- package-lock.json mnemo2/package.json`).
2. `git restore --source=origin/dependabot/<branch> -- mnemo2/package.json package-lock.json`.
3. `npm ci` at the checkout root to sync node_modules to that lock.
4. Verify with `npm ls <pkgs> --workspace mnemo2 --depth 0` and `npx --no-install tauri --version` in `src-tauri/mnemo2-app`.

Gotcha: jsdom is pinned by the workspace-root `overrides` in `package.json` (`31.0.0`, hoisted by commit c4e34d80 so vitest resolves it on a clean `npm ci`). A mnemo2 devDependency of `31.0.1` therefore still installs jsdom 31.0.0 — Dependabot's own PR has that same outcome; changing the root override is outside a dependency-folding step.
