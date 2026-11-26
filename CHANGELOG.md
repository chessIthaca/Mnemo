# Changelog

All notable changes to Mnemo are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versioning follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Each release also
gets GitHub release notes; the installers are built by
`.github/workflows/build.yml` when a `v*` tag is pushed (release steps:
`.coding/knowledge/how/2026-11-12-cut-a-mnemo-release-with-the-security-and-depend.md`).

## [2.10.0] — 2026-11-26

Worker hosts, so Mnemo workers (and Claude Code workers) run on a second Windows or Mac computer.

### Added
- Worker hosts: pair another Windows or Mac computer with `mnemo2-node` and the hub places workers there, so their builds and checks run on that machine. Code travels as git bundles, model calls and shared memory stay on the hub, and the host needs no API keys.
- **Settings → Worker hosts**: pair a host with a one-time code, link it to an SSH connection, unlink and revoke. Each host shows its operating system, connection state, check result and busy slots.
- Claude Code workers on a linked host: the lead starts the session over the desktop app's SSH connection to the host. Linking a host is your approval, and Mnemo never launches `claude` itself.
- Every worker shows its host: a badge such as `studio` in the worker list and `host: studio` in its detail, with an away state and time while the host is unreachable.
- When a host goes away, Mnemo workers continue if it returns within the outage budget, Claude Code sessions keep working, and `team_resume` continues workers after a failure or a host restart.
- A host keeps one to eight warm checkouts per project (`--slots`, default 2).
- The `mnemo2-node` binary ships in the Windows installer and the Mac app bundle, and `docs/WORKER-HOSTS.md` walks through installing and pairing a host.

### Changed
- Git-heavy hub-to-host calls (bundle upload, checkout and fetch) wait up to 180 seconds, so a large repository or a busy host no longer fails with a timeout.
- A merged worker's checkout on its host is released automatically, a reused dispatch names the host's live SSH connection, and a finished worker drops its away state.
- Development check runs bind only loopback, so they raise no firewall prompt.

### Fixed
- A host trims only idle checkouts and never one a worker is still using.

## [2.9.0] — 2026-11-25

Claude Code agent workers start without a permission prompt and are handed back more patiently, and Mnemo2 shuts down cleanly.

### Added
- A Claude Code agent worker's launch is approved without a permission prompt: Mnemo puts a one-time token on the first line of each launch prompt it hands the lead, and the gate approves only a launch carrying a live token, once. It covers worker launches and records only, and a launch without the token falls back to Claude Code's own rules.
- An explicit code-embedder option for the engine: automatic is the default, and off never builds or downloads the code model.

### Changed
- An agent worker goes back to Mnemo's models only after its Claude Code lead has been away for a grace period (`[routing] agent_hand_back_minutes`, default 30, 0 hands back at once), and Mnemo looks for the agent before handing an unlaunched worker back.
- A non-isolated agent launch while a worker waits for its agent is denied with a reason that says how to relaunch it.
- Test temp folders stay inside the checkout's `target` folder instead of the system temp folder, and killed test command trees are reaped so folders stop leaking.
- Dependencies: svelte 5.59.1 (#154).

### Fixed
- Shutdown waits for aborted tasks and the last engine handle, and releases the memory store and the code graph.
- Memory consolidations finish detached and fit within the app's shutdown deadline.
- In-memory store readers take no shared-cache table locks, so a background write no longer fails with a "table is locked" error.
- A full test run no longer downloads the code model into a temp folder.

## [2.8.0] — 2026-11-23

A tier can run as a Claude Code agent, code search finds code by meaning, and one shared model host serves every window.

2.7.0's installers were never published, so 2.8.0 is the first release after 2.6.0 and carries 2.7.0's changes too.

### Added
- A tier can run as a Claude Code agent: **Run as** on the Tiers page picks Mnemo's own model or a Claude Code agent (Opus, Sonnet, Haiku, Fable) with its own effort, and a step that lands on such a tier is handed to that agent instead of a Mnemo worker.
- Claude Code agent workers are registered, watched and heard from: Mission Control shows an agent's phase, log, activity and its real model, the workers band names its agent, and `team_message`, `team_route_fix` and `team_integrate` reach it.
- Up to three isolated agent workers work at once, spawned one at a time, each in a pool checkout that its shell already starts in; its screenshots survive the checkout's release.
- `find_code` searches code by meaning: describe what a function does and it blends the code model's vectors over every stored symbol (signature and doc text included) with the keyword index and a name match.
- `team_run_checks` runs the whole check battery for the team while a fix round verifies narrowly, and workers record their lead base, follow the lead while idle and report a stale one.
- `team_tiers` reads and changes the tiers, with a warning when a change reaches running work and when a dispatch bypasses a guardrail.
- One `mnemo-models` host per machine serves the embeddings over loopback to every window, under an unguessable token; **Settings → Machine** and **Settings → Models** show its status and what each model is for.
- A changed memory model re-embeds the store automatically, resumes only the rows still stale, and the status line shows the run.

### Changed
- `job` replaces `job_start`: one tool starts a long command and can wait for its result, and `job_wait`, `job_tail` and `job_kill` drive it from there.
- A window's tool calls say what the call is for instead of "Using Bash", built from the arguments as they stream in, and `job` calls read like shell calls.
- Used audits show as done in Mission Control, a finished audit no longer counts as waiting on you, and a stop closes the audit and refreshes the board.
- A model that hit its usage limit is skipped until it resets, and the limit holds through an escalation, a resume and a refusal.
- An agent's `git stash` is refused: the stack is shared with other workers, so park work in a WIP commit instead.
- Free pool checkouts above the worker slot count are trimmed as a soft cap, and an idle one is removed in the background.
- A bug plan records its regression test by the bare test name.
- Dependency updates: rusqlite 0.43, tree-sitter-c-sharp 0.24, clap 4.6, svelte 5.59, vitest 5.1, the Tauri CLI and API 2.12, and the CodeQL actions (#146–#151).

### Fixed
- An npm check in a checkout that never installed its dependencies installs them first instead of failing.
- `run_checks` reports failing tests above git CRLF warnings in a FAIL block.
- A check run no longer fails on the lead's own `.coding/` bookkeeping change, and the lead's tool list refreshes when the tiers change or Mnemo2 restarts.
- Agent workers survive a restart, a short run finishes, a merged one has nothing left to continue, and prompts, notices and refusals suit a Claude agent.
- A dropped regression proof kills and reaps its `git apply` before restoring the tree.

## [2.7.0] — 2026-11-19

Workers run long builds in one call, edit more reliably, prove their tests, and several Mnemo2 windows share one machine.

### Added
- Background jobs: `job_start` runs a build or test as a session-owned job, `job_wait` waits server-side for its exit or a matching line, `job_tail` shows progress and `job_kill` stops the whole process tree. Nothing a job starts outlives its session.
- `run_checks` runs every `.coding/checks.toml` command in one call, reports pass or fail per check with the first errors and failing tests, and records the passing runs as merge checks.
- Mnemo2 proves a bug fix's regression test fails without the fix, and for high-risk work runs a mutation check that reports changed code no test catches as findings.
- Recurring review findings become proposals to enforce them, and the Quality tab shows how merged work holds up per tier and model, plus edit attempts and failures by cause.
- Several Mnemo2 windows share one machine:
  - one Laya server for all of them, ended by a watchdog once no window uses it;
  - a machine-wide limit on heavy builds and tests (Settings → Machine);
  - a per-endpoint "Max requests at once" limit and a shared pause after a rate-limit error;
  - an automatic shared Rust build cache when sccache is installed;
  - an overview of every running window in Mission Control;
  - HOW and DECISION notes in `~/.mnemo/knowledge` that every project finds.
- Hand edits of `~/.mnemo/*.toml` reach every running window without a restart.
- Workers check visual changes with the image tools, and Settings → Models warns when the vision model cannot be used.

### Changed
- A shell command is killed after 20 minutes without output or 60 minutes in total, instead of after a fixed 5 minutes, and a killed command keeps the end of its output.
- Memory search finds records by meaning, not only those whose words match.
- Read receipts follow a worker's own earlier edits, a near-miss edit returns the current text with a fresh receipt, and misshapen tool arguments are repaired or explained with an example call.
- Stale-read redirects no longer count toward the escalation nudge.

### Fixed
- `review_fix` matches line endings like the worker edit tool.
- A missing mutation tool is reported once as a notice instead of as a worker error.
- The memory index updates live when `.coding/` files change, and a leading UTF-8 BOM no longer breaks the record parsers.
- Frontend tests pass on a fresh install again (jsdom pinned to 31.0.0).

## [2.6.0] — 2026-11-16

Workers build faster, and the lead stops running every test suite twice.

### Added
- Workers and audits reuse checkout folders (`.worktrees/pool-<n>`), so their build caches stay warm and a new worker's builds are incremental instead of starting from scratch.
- A worker's own passing runs of the commands listed in `.coding/checks.toml`, run exactly as written in its checkout on the code it then commits, count as merge checks; the lead no longer reruns them.
- **Settings → Claude Code → Connect** also installs `mnemo-workers`, the band that lists the running workers above the prompt in Claude Desktop.

### Changed
- Shell results show stderr before stdout, so an error stays visible when a long output is shortened.
- The worker check list and CI drop the `-p mnemo-app --features mnemo2` runs that only repeated the workspace runs.
- Dependency updates: rusqlite 0.42, fastembed 7.7, vite 8.3.8 and smaller patch releases.

### Fixed
- An audit that has not reported yet can switch its model from mission control.
- `plan_read` with an unknown capability names the real ones and points the worker back to its plan.
- An audit that ends with its findings as plain text still delivers them as its report and frees its worker slot.

## [2.5.0] — 2026-11-15

Mnemo2 keeps itself up to date. Installs of 2.4.0 or older need this release once by hand; from 2.5.0 on, updates arrive on their own.

### Added
- Mnemo2 updates itself: it checks for a new release at start and every 6 hours, downloads it in the background and installs it when you click **Restart now**. Releases publish signed update files and `latest.json`. (#133)

## [2.4.0] — 2026-11-15

Claude Code is ready to lead straight from the installers.

### Added
- The installers ship the Claude Code plugin's `mnemo2-mcp` helper. Mnemo2 installs the plugin and helper into `~/.mnemo/claude-plugin` at every start, and **Settings → Claude Code → Connect Claude Code** registers the plugin with Claude Code in one click, so leading with Claude Code no longer needs a source build. (#131)

### Changed
- The README Quickstart explains how to lead Mnemo2 from Claude Code (#130).

## [2.3.1] — 2026-11-14

The documentation describes Mnemo2 only, plus two fixes.

### Changed

- The README, `docs/FEATURES.md`, `docs/CONFIGURATION.md`, `docs/REVIEW-POLICY.md` and `mnemo2/README.md` describe Mnemo2 only; the README adds install-from-release and first-run steps (#127).
- The README's first-run steps cover choosing the semantic-search model and the optional Laya install.
- `agent.md`: the repository's test and review rules now reach Mnemo2's lead and workers, and the landing rules name the current `main` rulesets (#127).

### Fixed

- Removing an endpoint's model that is the configured default no longer fails: the default moves to the endpoint's next model (#126).
- When Claude Code leads, `search` and `search_read` record stale-index repairs in `index-staleness.jsonl` (#128).

## [2.3.0] — 2026-11-12

Mnemo2 is now the only Mnemo app. Mnemo1, the original desktop app, is retired: its code is gone and its installers no longer ship.

### Added
- Slash commands in Mnemo2's lead composer: `/new`, `/clear`, `/compact`, `/model`, `/stop` and `/help` (#120).
- The last Mnemo1 app features Mnemo2 still lacked (#117).
- Every pull request now runs the build, clippy and the tests (#118).

### Changed
- The Rust toolchain is pinned to 1.99.1 in `rust-toolchain.toml` (#119).
- Settings → Tiers only sets how Mnemo2 picks a worker's tier; the Mnemo1 routing switch, the Reflex option and the mismatch warning are gone (#122).
- Dependency updates, among them rusqlite 0.41, tree-sitter-php 0.25, jsdom 31 and the CodeQL actions (#108–#113, #116, #117).

### Removed
- Mnemo1: the `frontend/` app, its Tauri binary, `start.bat`, `build.bat` and its installers (#120).
- About 5,500 lines of Mnemo1-only core code and dead settings; retired keys in an existing `config.toml` are ignored (#121).

### Fixed
- Mnemo2 workers are no longer moved to another model mid-plan by the per-step reflex lane (#122).
- A flaky model-switch test (#122).
- Backlog items that Mnemo1's Run-All left "With the lead" are closed (#123).

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
