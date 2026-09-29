# Changelog

All notable changes to Mnemo are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versioning follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Each release also
gets GitHub release notes; the installers are built by
`.github/workflows/build.yml` when a `v*` tag is pushed (procedure:
`.coding/skills/new_release.toml`).

## [1.3.0] — 2026-09-29

The GitHub-reporting sweep: every open Dependabot alert and CodeQL
code-scanning finding was closed, and the dependency set modernized across the
board. Full snapshot with per-finding detail:
`.coding/analysis/2026-09-29-github-security-defects.md`.

### Changed
- **Dependencies modernized** (PRs #16–#60): React 19, TypeScript 7.0,
  Tailwind CSS 4.3 (`@tailwindcss/postcss` replaces `autoprefixer` — 62 files
  migrated with the official upgrade tool), Vite 8, vitest 5, zustand 5,
  react-markdown 10, lucide-react 1.48, chromiumoxide 0.9 (off the removed
  `HeadlessMode`), rusqlite 0.40, tiktoken-rs 0.12, sha2 0.11, toml 1.1, zip 8,
  windows-sys 0.61, directories 6, plus the GitHub Actions bumps (checkout 7,
  setup-node 7, upload-artifact 7, download-artifact 8, action-gh-release 3).
- The vendored tauri pins (`tao`, `wry`, `tauri-runtime-wry`) stayed on their
  `[patch.crates-io]` path overrides through every bump — `Cargo.lock` carries
  no `[[patch.unused]]` entries.

### Fixed
- **React #185 render loop at startup** — zustand 5 dropped the
  `useSyncExternalStoreWithSelector` wrapper; the store now builds with
  `createWithEqualityFn` from `zustand/traditional` (#58).
- The nested-button DOM error in the context popup and the
  uncontrolled→controlled Radix Tabs warning (#60).

### Security
- Closed all 13 open CodeQL findings (#31): workflow token permissions, a
  `Math.random()`-derived React key, two ReDoS-prone regexes (the heuristic one
  validated by a 310,000-command differential fuzz plus a timing table), four
  cleartext-logging test assertions, and the production fix that stops
  `consolidation.rs` logging the session id. Three `rust/insecure-cookie`
  findings were dismissed as false positives (the vendored wry propagates
  `Secure`). The open Dependabot alert (`glib`, GHSA-wrw7-89jp-8q8g) was
  dismissed as unreachable — a Linux/GTK-only lock entry never compiled for the
  Windows/macOS targets.
- Every GitHub Action ref is pinned to a full commit SHA — the one documented
  exception is `dtolnay/rust-toolchain@stable`, which selects its toolchain by
  ref — guarded by `workflows_pin_actions_to_full_shas` in
  `tests/integration/ci_workflow.rs`;
  the build workflows scope `GITHUB_TOKEN` to `contents: read` while the
  release job keeps its `contents: write` override (#41, plus the same-day
  repo-hygiene merge).
- CodeQL runs in **advanced setup** (`.github/workflows/codeql.yml`, weekly,
  `vendor/**` excluded); `.github/dependabot.yml` adds grouped weekly updates
  (the vendored `tao`/`wry` are ignored by design) and `SECURITY.md` documents
  the policy.

### Added
- `.coding/knowledge/how/…-read-the-running-app-webview-console-via-cdp.md` —
  how to attach to the running WebView2 over Chrome DevTools Protocol (the
  technique that diagnosed the #185 loop) (#59).

## [1.2.0] — 2026-09-27

Agentic harness release with Anthropic and Laya model support — **published
without installers**: the tag build's macOS runner hit the 10 s `shutdown_agent`
window (`state_override_survives_429_fallback`), the `release` job was skipped,
and the GitHub release went out with no assets. The window was widened to 30 s
(`4b0764a`) and the failed jobs re-run. **1.3.0 supersedes this release.**

## Earlier

`v0.1.1` (2026-09-21) and `WindowsRelease` (2026-09-13) — see the GitHub
releases page.

[1.3.0]: https://github.com/chessIthaca/Mnemo/releases/tag/v1.3.0
[1.2.0]: https://github.com/chessIthaca/Mnemo/releases/tag/v1.2.0
