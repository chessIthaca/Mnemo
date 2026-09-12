# Research: macOS (Apple) port of myharness

Date: 2026-08-20. Plan: de68c811 (kind: research). **No source changes were made.**

## TL;DR

**Effort class: MEDIUM.** The framework (Tauri 2) and the overwhelming majority of the
codebase are already cross-platform by construction — the Rust "brain" (`src/`) compiles
cleanly for macOS today: every OS-specific block is `#[cfg(windows)]`-gated and the Unix
branches already exist. There is exactly **one genuinely Windows-locked subsystem**: the
live WebView2 + CDP game-inspection stack (`game_*` tools). Everything else is packaging,
one macOS environment gotcha (`$PATH` for GUI apps), and dev-machine docs.

Realistic phasing: **M1** "compiles + runs on a Mac" ≈ a day of on-device work;
**M2** "functional parity minus live-CDP inspection" small; **M3** "signed, notarized
DMG via CI" is process/cost (Apple Developer Program, $99/yr), not engineering.

## Per-subsystem assessment

| # | Subsystem | Status on macOS | Evidence / work needed |
|---|-----------|-----------------|------------------------|
| 1 | Brain lib (`src/` — agent, provider, memory, workflow, tools, project) | **Portable as-is** | `windows-sys` is target-gated (`Cargo.toml:76-82`). `keys.rs` has the Unix `0o600` branch (`src/config/keys.rs:140-160`) plus a `not(any(unix,windows))` fallback. `global_config_dir()` uses `directories::BaseDirs::home_dir` → `~/.myharness` on mac (`src/config/mod.rs:362-374`). Trace-log permission hardening shares the same dual-platform `restrict_permissions`. |
| 2 | `shell` tool | **Portable as-is** | Chooses `powershell -Command` vs `sh -c` via `cfg!(target_os = "windows")` (`src/tool/agent/shell.rs:136-140`); `CREATE_NO_WINDOW` is `#[cfg(windows)]`-gated (:152-156). `sh` exists on macOS. Tool description already says "On Windows uses PowerShell; on Unix uses sh". |
| 3 | Safety command classifier (`cmd_class.rs`) | **Degrades safely; small optional work** | The classifier is PowerShell-flavored (backtick subexpressions, `Select-String` filters). Under `sh`, unclassifiable commands return `None` → they always prompt (safe). Common commands (`cargo test`, `git …`) classify fine on both. Optional: teach it sh-style filter segments (`\| grep`, `\| head`, `2>&1`) so "Mark Safe (same operation)" works for sh pipelines. ~small. |
| 4 | `git` / `git_diff` tools, `project::git_ops`, `ipc/files.rs` spawns | **Portable as-is** | Discrete-argv spawns, no shell; all `CREATE_NO_WINDOW` uses are `#[cfg(windows)]`-gated (git.rs:105-109, git_diff.rs:47-51, git_ops.rs:26-30, files.rs:567-572,668-673,797-801). |
| 5 | Console REPL mode (`-console`) | **Portable as-is** | `attach_parent_console` has a `#[cfg(not(windows))]` no-op twin (`src-tauri/src/console.rs:1473-1474`). A terminal-first fallback if GUI work ever lags. |
| 6 | Headless `browser_*` agent tools | **Portable (needs Chrome on the Mac)** | chromiumoxide 0.7 launches/detects Chrome or Chromium per-platform (standard macOS install paths + env override). Tests already launch real Chromium cross-platform (`src/browser/mod.rs:1501+`). |
| 7 | Memory embeddings (fastembed/ONNX) + SQLite + file watcher | **Portable as-is** | fastembed 4 (ort) ships macOS aarch64 + x86_64 builds; `rusqlite` bundled; `notify` 8 uses FSEvents/kqueue on mac. First-run model download is runtime behavior, unchanged. |
| 8 | Frontend (`frontend/`, React+Vite) | **Portable as-is** | Pure web. Path handling is already separator-agnostic (`frontend/src/lib/language.ts:38`, `toolCardPaths.ts:10` normalize `\\` → `/`; tests cover Windows backslashes). Vite/tsc/vitest run on macOS unchanged. |
| 9 | Main window + child webview (`WindowBuilder` + `add_child`) | **Supported; verify on device** | `Window::add_child` is "Available on desktop and crate feature `unstable` only" (docs.rs, tauri 2.11.5 — desktop = Windows/macOS/Linux; the project already enables `unstable`). On macOS wry embeds a WKWebView as an NSView subview. The rect-reporting/overlay/show-hide machinery in `src-tauri/src/ipc/browser_webview.rs` is OS-agnostic Tauri API calls — expected to work, but z-order/punch-through behavior must be verified on real hardware (the Windows HWND commentary in the module is cosmetic). |
| 10 | Windows-only leftovers in shared code | **Harmless no-ops; tidy-up optional** | `main.rs:95` sets `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` unconditionally — ignored by wry on macOS (worth a `#[cfg(windows)]` for clarity). `windows_subsystem` attr (`main.rs:6`) is a no-op off Windows. `webview_args.rs` is pure string assembly (compiles anywhere; occlusion/RDP flags are Windows concerns). `thread_util.rs` is generic. |
| 11 | **Live WebView2 CDP stack** (`game_*` tools, `src/browser/mod.rs:647+`, CDP port 9222) | **THE port — Windows-locked** | WKWebView has **no CDP** and no `--remote-debugging-port` equivalent. `WKWebView.isInspectable` (macOS 13.3+) exposes the webview to Safari's Develop menu — *human-driven* debugging only, no programmatic protocol the agent can attach to. See "The crux" below. |
| 12 | Packaging / signing / CI | **Process, not code** | `tauri.conf.json` already ships `icons/icon.icns` + `bundle.targets="all"`. Needs a `bundle.macOS` section, a Developer ID cert, notarization, and a CI workflow (none exists today — no `.github/`). See "Packaging & distribution" below. |

## The crux: `game_*` tools without CDP

The six live-inspection tools (`game_screenshot/eval/snapshot/navigate/click/type`,
registered unconditionally in `src/agent/factory.rs:658-663`) attach over CDP to the
app's own child WebView2 — the surface the *human* is interacting with. On macOS the
child webview is a WKWebView, which offers no CDP. Ranked options:

**Option A — degrade gracefully (recommend for M1). Effort: small.**
`#[cfg]`-gate the CDP attach; on macOS the `game_*` tools either aren't registered or
return a clear "not supported on this platform" error. The agent falls back to the
headless `browser_*` tools (which work on macOS). Loses exactly one capability:
inspecting/driving the human-visible Browser tab.

**Option B — in-process transport (parity path). Effort: medium; screenshot is the hard part.**
The app *owns* the child webview, so most `game_*` verbs don't need CDP at all:
- `game_eval`, `game_navigate`, `game_snapshot` → `tauri::webview::Webview::eval`
  (cross-platform; navigate already has an eval-based precedent in `browser_webview.rs`).
- `game_click`, `game_type` → JS-dispatched input events via eval (slightly less
  faithful than CDP input, workable).
- `game_screenshot` → needs `WKWebView.takeSnapshot` via Objective-C (`objc2-web-kit`
  is already linked transitively by Tauri, but wry does not publicly expose the raw
  WKWebView pointer — this may require a wry patch or unsafe msgSend through the
  platform handle). Do this one last.

**Option C — embed Chromium instead of WKWebView (CEF-style). Rejected.**
Not a wry capability; would mean a different webview stack on macOS only. Cost/risk
far outweighs the benefit.

## Packaging & distribution (from Tauri v2 docs, 2026-08-20)

- **Build**: `npm run tauri build -- --bundles app,dmg` on a Mac. Bundle layout,
  Info.plist auto-generated; extend via `src-tauri/Info.plist` (merged). Icon already
  present (`icon.icns` in `tauri.conf.json:26`).
- **Config gaps**: add `bundle.macOS` — `category` (`public.app-category.developer-tools`),
  `minimumSystemVersion`, optionally `signingIdentity`, `entitlements`. **Do NOT enable
  the App Sandbox**: the app spawns child processes (git, sh, Chrome) and does arbitrary
  localhost networking — sandboxing would break its core function. Developer ID +
  notarization is the right path.
- **Signing**: Apple Developer Program ($99/yr) → *Developer ID Application* certificate.
  Configure via `APPLE_SIGNING_IDENTITY` env or `bundle.macOS.signingIdentity`. Free
  Apple accounts can't notarize (app shows "not verified"). Ad-hoc signing runs on
  Apple Silicon but Gatekeeper warns — fine for personal dev builds only.
- **Notarization** (required with Developer ID): App Store Connect API key
  (`APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_PATH`) or Apple ID app-specific
  password + `APPLE_TEAM_ID`; Tauri runs `notarytool` and staples the ticket to the DMG.
- **CI**: no CI exists today. GitHub Actions `macos-latest` + `tauri-apps/tauri-action@v0`,
  matrix `--target aarch64-apple-darwin` (Apple Silicon) + `--target x86_64-apple-darwin`
  (Intel) — or a `universal2-apple-darwin` single binary. Cert import via the documented
  keychain dance (`APPLE_CERTIFICATE` base64 .p12 + `KEYCHAIN_PASSWORD`). Note: macOS
  runner minutes are 10× on private repos.
- **No updater**: the app doesn't use `tauri-plugin-updater` today, so no update-signing
  key management is needed (distribution is a DMG the user replaces).

## The one macOS functional gotcha: `$PATH`

GUI apps on macOS do **not** inherit the shell's `$PATH` (no `.zshrc`) — Finder-launched
apps get `/usr/bin:/bin:/usr/sbin:/sbin`. The `shell` tool spawns `sh -c …`, so
agent-run `cargo`, `npm`, `node`, Homebrew `git`, etc. would not be found when the app
is launched as a bundle (terminal launch is fine). Tauri's own docs call this out and
recommend **fix-path-env-rs**. Adopting that at startup (Unix-only) is a **must-fix**
for the Mac version — without it the agent's core loop is broken in the shipped .app.

## Dev-environment / constitution changes

- `agent.md` (project constitution) hardcodes Windows 11 + PowerShell + `C:\` paths —
  it's per-project dev-machine policy, not app code. For cross-platform development it
  needs a per-OS split (Windows section stays; add macOS: zsh/sh, `$HOME` paths; the
  "don't trust piped exit codes" rule is PowerShell-specific — on sh use `echo $?`).
- The global system prompt already says the right thing ("On Windows uses PowerShell;
  on Unix uses sh") via the shell tool description.
- Windows-only hardening (RDP/occlusion workarounds, `MYHARNESS_DISABLE_WIN_OCCLUSION`)
  simply doesn't apply on macOS — no port needed, just docs noting they're Windows-only.

## Suggested phased plan

- **M1 — compile & run on a Mac (~1 day on-device)**: rustup + Xcode CLT + Node; fix
  compile fallout (expect ~zero; likely just cfg-gating the WEBVIEW2 env-var set);
  `fix-path-env` at startup; verify `add_child` child webview renders/moves/shows/hides;
  `cargo test` green; `game_*` degrade (Option A). Console REPL as smoke-test fallback.
- **M2 — functional parity minus live CDP**: Browser tab (WKWebView child) verified
  against the rect/overlay machinery; optional sh-filter support in `cmd_class`;
  agent.md per-OS generalization; manual smoke of approvals, memory, backlog, run-all.
- **M3 — distribution**: `bundle.macOS` config; Developer ID cert + notarization;
  GitHub Actions macOS matrix producing signed DMGs. Optional stretch: Option B
  eval-transport for `game_eval/navigate/snapshot` (+ `takeSnapshot` native work last).

## Not verified (needs real hardware)

wry child-webview z-order/resize behavior on macOS; fastembed first-run model download
on mac; chromiumoxide Chrome auto-detection paths; memory-DB performance parity;
whether `cargo test` is genuinely green on aarch64-apple-darwin (expected, unproven).
