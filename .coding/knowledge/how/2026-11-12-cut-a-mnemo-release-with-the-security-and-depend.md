+++
title = "cut a Mnemo release with the security and Dependabot fixes folded in"
supersedes = "2026-11-05-cut-a-mnemo-release-with-the-security-and-depend"
created = "2026-11-12"
evidence = "v2.3.0 release prep, plan 28eff481 (commit fe270f55): git grep of 2.2.0 found exactly these eight manifests after frontend/ and AboutDialog.tsx were deleted in #120; build.yml release job files dist/windows/**/*.msi|*.exe and dist/macos/**/*.dmg|*.zip."
+++

Before tagging a Mnemo release:
1. **Check GitHub for open alerts and PRs:**
   - security alerts: `gh api repos/chessIthaca/Mnemo/dependabot/alerts?state=open`, `code-scanning/alerts` and `secret-scanning/alerts`;
   - open Dependabot PRs.
2. **Fold the fixes into the lead working branch** rather than merging those PRs into main. Use targeted updates only: `cargo update -p <crate> --precise` and `npm install <pkg>@^x -w <workspace>`.
3. **Respect the dependency constraints:**
   - The workspace vendors tauri/wry (`[patch.crates-io]`), so Cargo.lock must keep no `[[patch.unused]]`. The guard is `cargo test --test integration cargo_lock_keeps`.
   - Keep @tauri-apps/plugin-* (npm) and tauri-plugin-* (crates) on the same major.minor.
4. **Bump the version.** Since Mnemo1 retired (v2.3.0 onward) it lives in eight manifests:
   - Cargo.toml, src-tauri/Cargo.toml, src-tauri/mnemo2-app/Cargo.toml;
   - src-tauri/tauri.conf.json, src-tauri/mnemo2-app/tauri.conf.json;
   - package.json, mnemo2/package.json, mnemo2/plugin/.claude-plugin/plugin.json.

   Then refresh the lockfiles:
   - `cargo update --workspace` must change only the mnemo, mnemo-app and mnemo2-app versions in Cargo.lock;
   - `npm install --package-lock-only` must change only the root and mnemo2 version fields in package-lock.json.
5. **Add a CHANGELOG.md section.**
6. **Land and tag:**
   - open a PR into main (main is PR-only) and merge it;
   - push an annotated v* tag on merged main;
   - .github/workflows/build.yml builds the Mnemo2 installers (Mnemo2_<v>_x64_en-US.msi, Mnemo2_<v>_x64-setup.exe, Mnemo2_<v>_aarch64.dmg, mnemo2.app.zip) and publishes the GitHub release.

The bump is a routine-tier Mnemo worker step, as v2.3.0's plan 28eff481 did.

Note: deepmerge (a low alert, via vite-plugin-svelte) had no fixed version in November 2026.
