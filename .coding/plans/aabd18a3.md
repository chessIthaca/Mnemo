# Plan: Release v2.8.0 with the open Dependabot updates folded in

## Goal
main carries version 2.8.0 in every manifest, today's dependency updates (the six open Dependabot PRs #146-#151) and a CHANGELOG 2.8.0 section, so tagging v2.8.0 on merged main builds and publishes the Windows and macOS installers.

## Kind
implementation

## Context
WHY: the v2.7.0 tag build (run 44646915241, 2026-11-20) failed on macOS-only shared_service tests (fixed later by PR #144), so no 2.7.0 release or installers were ever published; the latest release is v2.6.0. main (2aefdb7f, PR #152 merged 2026-11-23) has 116 commits beyond the v2.7.0 tag. The user chose v2.8.0 (no rewriting of the existing v2.7.0 tag) and chose to fold the open Dependabot PRs into the release. Procedure: .coding/knowledge/how/2026-11-12-cut-a-mnemo-release-with-the-security-and-depend.md and .coding/skills/new_release.toml. main is PR-only; CI (ci.yml) runs Windows only; build.yml (workflow_dispatch or v* tag) builds Windows + macOS installers and its release job publishes on a v* tag only.

DEPENDENCY UPDATES (step 1), each targeted, never a blanket update:
- #151 rusqlite 0.42 -> 0.43 (Cargo.toml line 84, keep features = ["bundled"]); adapt any API break in the ~41 rusqlite:: call sites.
- #150 tree-sitter-c-sharp 0.23.5 -> 0.24.0 (Cargo.toml line 139); used at src/codegraph/extract.rs:286 (`tree_sitter_c_sharp::LANGUAGE.into()`); adapt if the grammar's node kinds changed so the codegraph C# extraction tests still pass.
- #149 cargo-minor-patch group via `cargo update -p <crate> --precise <ver>`: clap 4.6.9, uuid 1.29.3, regex 1.13.2, zeroize 1.9.2, zip 9.8.1, tree-sitter-php 0.25.2, libc 0.2.191, tauri-build 2.7.6.
- #148 npm-minor-patch group in the mnemo2 workspace (`npm install <pkg>@^<ver> -w mnemo2`, then package-lock): @lucide/svelte 1.77.0, @tauri-apps/api 2.12.5, dompurify 3.4.18, @tauri-apps/cli 2.12.6, jsdom 31.0.1, svelte 5.59.0, vitest 5.1.1. Keep @tauri-apps/plugin-* (npm) and tauri-plugin-* (crates) on the same major.minor.
- #147/#146 .github/workflows/codeql.yml: both `github/codeql-action/init@...` and `github/codeql-action/analyze@...` pins change from 33d7b85da1a04171e211befe2c739303b9a4c066 to 1bd86f2fdb4b86187fa4059f16f31a3202acc9ea (keep the `# v4` comment).
CONSTRAINTS: the workspace vendors tauri/wry via [patch.crates-io]; Cargo.lock must keep no [[patch.unused]] (guard: `cargo test --test integration cargo_lock_keeps`). After `npm install`, `npx --no-install tauri --version` in src-tauri/mnemo2-app must print tauri-cli 2.12.x.

VERSION BUMP (step 2) to 2.8.0 in exactly these eight manifests: Cargo.toml, src-tauri/Cargo.toml, src-tauri/mnemo2-app/Cargo.toml, src-tauri/tauri.conf.json, src-tauri/mnemo2-app/tauri.conf.json, package.json, mnemo2/package.json, mnemo2/plugin/.claude-plugin/plugin.json. Then `cargo update --workspace` must change only the mnemo, mnemo-app and mnemo2-app versions in Cargo.lock, and `npm install --package-lock-only` only the root and mnemo2 version fields in package-lock.json. CHANGELOG.md: add `## [2.8.0] — 2026-11-23` above the existing [2.7.0] entry (keep that entry; add one line in 2.8.0 that 2.7.0's installers were never published, so 2.8.0 is the first release after 2.6.0 and includes 2.7.0's changes), summarizing what landed since the v2.7.0 tag (`git log --oneline v2.7.0..HEAD --no-merges`, skipping chore(coding) bookkeeping) in the existing section style, grouped by theme, plus a Dependencies line for the step-1 updates.

VERIFY (both steps): the four .coding/checks.toml commands from the checkout root; step 1 also `cargo test --test integration cargo_lock_keeps` and the tauri CLI version check; step 2 also `git diff --stat` touching only the manifests, the two lockfiles' version fields and CHANGELOG.md.

## Acceptance scenarios
- On the release branch, every manifest reads 2.8.0, Cargo.lock/package-lock.json carry the Dependabot versions, and the four checks.toml commands pass.
- A manual build.yml run (workflow_dispatch) on the release branch passes on both the Windows and the macOS job before the tag is pushed.
- After the PR merges and the annotated tag v2.8.0 is pushed on merged main, the build workflow publishes release v2.8.0 with Mnemo2_2.8.0_x64_en-US.msi, Mnemo2_2.8.0_x64-setup.exe, Mnemo2_2.8.0_aarch64.dmg and mnemo2.app.zip.

## Steps
- [x] 1. **Fold in the Dependabot updates** — files: Cargo.toml, Cargo.lock, mnemo2/package.json, package-lock.json, .github/workflows/codeql.yml, plus only the source files a rusqlite 0.43 or tree-sitter-c-sharp 0.24 API change forces (expected: none or a few call sites; src/codegraph/extract.rs for C#). Apply exactly the updates listed in the context, targeted. If rusqlite 0.43 or tree-sitter-c-sharp 0.24 needs a non-trivial code change (more than a few mechanical call-site edits) or breaks a test you cannot fix mechanically, STOP and message the lead with the failure instead of rewriting logic. Tests: the existing suites are the proof (codegraph C# extraction tests, all rusqlite-backed store tests via cargo test --workspace), plus `cargo test --test integration cargo_lock_keeps`. DONE: all updates applied, all four checks.toml commands pass, the guard passes, the tauri CLI check prints 2.12.x; one commit; do not touch any version field or CHANGELOG.md (step 2 owns them).
- [x] 2. **Bump to 2.8.0 and write the CHANGELOG section** — files: the eight manifests named in the context, Cargo.lock and package-lock.json (version fields only), CHANGELOG.md. Follow the context's VERSION BUMP rules exactly, including the lockfile-scope checks (report the `git diff --stat` and that Cargo.lock/package-lock.json changed only version lines). CHANGELOG: read the existing [2.7.0] and [2.6.0] sections for style, then write [2.8.0] from `git log --oneline v2.7.0..HEAD --no-merges` (skip chore(coding) commits), grouped by theme, concise user-facing lines, ending with a Dependencies line naming the step-1 updates. DONE: four checks.toml commands pass; one commit; touch no source file.

## Decisions
- 2026-11-23: no spec impact: Release housekeeping: dependency and version bumps plus CHANGELOG; no specified behavior changes.

## Interfaces
- 2026-11-23: Step order and file ownership: step 1 owns dependency entries in Cargo.toml/Cargo.lock/mnemo2/package.json/package-lock.json and .github/workflows/codeql.yml; step 2 runs AFTER step 1 is merged and owns only the version fields of the eight manifests and the two lockfiles, plus CHANGELOG.md. Never dispatch them in parallel.

## Progress
- 2026-11-23 step 1 done: step 1 merged: depsFold280 (depsFold280) on deepseek-v4.1-flash:1017, merge commit ff199376; review: The six Dependabot updates (#146-#151) are folded in with targeted updates: rusqlite 0.43, tree-sitter-c-sharp 0.24, the cargo and npm minor/patch groups, CodeQL action pins. No source changes were needed; no version fields touched.
- 2026-11-23 step 2 done: step 2 merged: versionBump280 (versionBump280) on deepseek-v4.1-flash:1017, merge commit b518f8d9; review: Version 2.8.0 in all eight manifests; Cargo.lock and package-lock.json change only the three/two version lines; CHANGELOG [2.8.0] section added above [2.7.0], noting 2.7.0 installers were never published.
