## Verdict: FINDINGS (0 high, 3 low)

**Axis:** macOS readiness/compatibility · **Round:** user-requested, 2027-01-06 · **HEAD:** 9a0d5b9 on wt/agenticcoding · **Reviewer:** read-only subagent.

**Yardstick applied:** the app must build and behave on both macOS and Windows; the ONE sanctioned exception is the WebView2 Browser tab / game_* tooling (Windows-only by design, gated). Note: no `game_*` tooling exists in the tree at HEAD (searched src/ and src-tauri/ — zero matches), so the sanctioned gate reduces to the WebView2 Browser tab stack, verified below.

**Summary:** The codebase is genuinely dual-platform — this is not a Windows app with macOS bolted on. Every `cfg(windows)`/`cfg(unix)` gate is either inside the sanctioned WebView2/browser gate or follows the standard cross-platform pattern (CREATE_NO_WINDOW flag, permission three-way, no-op fallback); the shell tool picks PowerShell vs `sh` at runtime; all paths go through `PathBuf` with backslash→slash normalization at the edges; `.gitattributes` pins the whole repo to LF; the bundle config ships `targets: "all"` with an `.icns` icon and macOS 11.0 minimum; CI builds and tests on both `windows-latest` and `macos-latest` with secrets-driven macOS signing/notarization; and `main.rs` even carries a macOS-specific fix (login-shell `$PATH` adoption for Finder/Dock launches). The three findings are dev-tooling/docs nits — nothing breaks the macOS build or app behavior.

---

## Findings

### LOW-1 — `npm start` is wired to a Windows-only script

- **Evidence:** `package.json:10` — `"start": "start.bat"`. `start.bat` is a batch file (`@echo off`, `cd /d "%~dp0"`, `call npm install`, then `node_modules\.bin\tauri.cmd`).
- **Why it matters:** On macOS, `npm start` runs scripts through `sh`, which cannot execute a `.bat` — it fails with `sh: start.bat: command not found`. A macOS developer's most instinctive entry point is dead. This does NOT break the documented macOS path (README gives explicit macOS commands and never says `npm start`), so it is a dev-convenience gap, not app behavior.
- **Fix direction:** Either (a) replace the alias with a cross-platform launcher (`"start": "node scripts/start.mjs"` doing the same npm-install sync + `npx tauri dev`/`build`), or (b) add a `start.sh`/`start.command` counterpart and document it in the README's macOS section, or (c) drop the `start` alias entirely and keep per-OS quick-start commands in the README (status quo is already documented that way).

### LOW-2 — Windows-only dev helper scripts with no macOS counterpart

- **Evidence:** `start.bat` (53 lines) and `build.bat` (33 lines) at repo root — both encode real workflow knowledge (npm sync before CLI use, tauri must run from `src-tauri/`, `--no-bundle` default vs `bundle` opt-in, exit-code plumbing). No `start.sh`/`build.sh`/`justfile`/`Makefile` counterpart exists. (`scripts/add-copyright-headers.ps1` and `scripts/relicense-to-mit.ps1` are one-time, already-applied maintenance scripts — no action needed there.)
- **Why it matters:** Constitution's neutrality rule covers library/app code, and these are dev tooling — but the developer experience is asymmetric: the Windows quick-start is one command, the macOS path is a manual sequence extracted from the README. The knowledge in the .bat files (especially "tauri CLI is hoisted to root node_modules and must run with src-tauri as cwd") lives only in batch-file comments, not in the macOS docs.
- **Fix direction:** Optional: add `start.sh` + `build.sh` (or a `justfile`) mirroring the .bat logic, or fold the key constraints into the README's macOS build section as a numbered command list. Lowest-effort acceptable fix: a short "macOS quick start" block in the README reproducing the .bat sequence as plain commands.

### LOW-3 — `debug.config.toml` gives only the Windows copy target

- **Evidence:** `debug.config.toml:3-4` — "Global config lives in `~/.mnemo/`. To use this for a debugging session: Copy this file to `%USERPROFILE%\.mnemo\config.toml`".
- **Why it matters:** Line 3 is neutral (`~/.mnemo/`) but the actionable instruction on line 4 is Windows-only — a macOS user following it literally copies to a wrong/nonexistent path. Trivial docs nit in a sample config, not app behavior.
- **Fix direction:** Add the macOS equivalent beside it: "…to `%USERPROFILE%\.mnemo\config.toml` (Windows) or `~/.mnemo/config.toml` (macOS)".

---

## Verification inventory (what was checked and passed)

### 1. Platform gates — every cfg in src/ and src-tauri/ (vendor/tao excluded as vendored third-party)

| Location | Gate | Verdict |
|---|---|---|
| `src/agent/factory.rs:924-953` | `#[cfg(feature = "browser")]` + `#[cfg(windows)]` browser tool registration | ✅ sanctioned gate |
| `src/agent/factory.rs:1950` | `#[cfg(windows)]` expected-tool-set extension in a test | ✅ sanctioned gate (test-side mirror) |
| `src/config/keys.rs:150,158,178,246,428` | `cfg(unix)` 0o600 / `cfg(windows)` DACL / `cfg(not(any(unix, windows)))` no-op — three-way permission hardening | ✅ exemplary cross-platform |
| `src/project/git_ops.rs:33` | `cfg(windows)` CREATE_NO_WINDOW for git spawns | ✅ standard pattern |
| `src/provider/trace.rs:2485,2516` | `cfg(unix)` test-only 0600 assertions | ✅ correct |
| `src/tool/agent/git.rs:407`, `git_diff.rs:56`, `git_read.rs:87` | `cfg(windows)` CREATE_NO_WINDOW | ✅ standard pattern |
| `src/tool/agent/shell.rs:220` | `cfg(windows)` CREATE_NO_WINDOW | ✅ standard pattern |
| `src-tauri/src/console.rs:1561` | `cfg(windows)` attach_console (raw FFI, no winapi crate) | ✅ gated |
| `src-tauri/src/console.rs:1675` | `cfg(not(windows))` no-op attach_console | ✅ graceful fallback |
| `src-tauri/src/console.rs:2612-2840` | `cfg(windows)` + `cfg(test)` console tests | ✅ gated |
| `src-tauri/src/ipc/files.rs:795,903,986,1100` | `cfg(windows)` CREATE_NO_WINDOW (4 git spawns) | ✅ standard pattern |
| `src-tauri/src/main.rs:46` | `cfg(windows)` browser_inspection_enabled | ✅ sanctioned gate |
| `src-tauri/src/main.rs:89` | `cfg(unix)` inherit_shell_path — adopts login shell `$PATH` for Finder/Dock launches (macOS GUI apps don't inherit .zshrc) | ✅ macOS-specific FIX, dependency-free for CI |
| `src-tauri/src/main.rs:115` | `cfg(unix)` is_adoptable_path helper | ✅ part of the above |
| `src-tauri/src/main.rs:185,193` | `cfg(windows)` WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS block | ✅ sanctioned gate, documented as such |
| `src-tauri/src/watchdog.rs` (many) | cross-platform core + `cfg(windows)` enrichment (stack walk, wait chain) with graceful degradation | ✅ exemplary |
| `src-tauri/src/ipc/browser_webview.rs` | runtime `cfg!(windows)` + red "unsupported" panel + backend safety net | ✅ sanctioned gate, degrades gracefully |
| `src/webview_args.rs` | pure string assembly, no Windows imports; only *called* from `cfg(windows)` code | ✅ compiles everywhere |
| `src-tauri/tests/tao_backport.rs` | reads vendored tao source files (present on all platforms) | ✅ platform-neutral test |

**Ungated Windows-only behavior:** none found. No `game_*` tooling exists at HEAD. No `std::os::windows`/`windows_sys`/`winapi` import exists outside a `cfg(windows)` block (verified by exhaustive search of both crates).

### 2. Hardcoded Windows assumptions

- **Backslash separators:** every `\` occurrence in src/ + src-tauri/ is either (a) normalization `.replace('\\', "/")` — the correct direction, e.g. `codegraph/mod.rs:223,529`, `ipc/files.rs:134,388`, `search.rs:325,1063`, `browser/mod.rs:1619` — or (b) test fixtures with `cfg!(windows)` branches (`shell.rs:519`, `sandbox.rs:387`, `browser/mod.rs:1706`). No production string builds a backslash path.
- **Config/home dirs:** `directories` crate + `HOME`/`USERPROFILE` fallbacks (`config/mod.rs` ~line 414) — cross-platform. Legacy migration `~/.myharness → ~/.mnemo` uses the same resolution.
- **Path construction:** `PathBuf::join` throughout; no `format!("{}/{}", …)` path concatenation found in production code.
- **CRLF:** `.gitattributes` pins `* text=auto eol=lf` repo-wide (uniform LF working tree on every platform); the `convert_line_endings` tool does CRLF↔LF in place; `file_append` normalizes; SSE parser is CRLF-tolerant (`mcp/http.rs:309,351`); knowledge-file parser handles CRLF. No parser assumes CRLF.
- **Globs:** `search.rs:143,162` splits on both separators and rejects `..` traversal on either; brace alternation expanded in code. No backslash globs.
- **Case sensitivity:** frontend path utilities dedupe case-insensitively (`toolCardPaths.ts:183` `.toLowerCase()`); macOS APFS default is case-insensitive, so no mismatch risk.

### 3. Shell and process assumptions

- **Shell tool** (`src/tool/agent/shell.rs:200-230`): runtime `cfg!(target_os = "windows")` → `powershell -Command` on Windows, `sh -c` otherwise. Correct dual-platform design; the "powershell" string at line 205 is inside the Windows branch.
- **Git invocations:** every git spawn across `git_ops.rs`, `git.rs`, `git_diff.rs`, `git_read.rs`, `ipc/files.rs` uses `Command::new("git")` with discrete argv — no `git.exe`, no shell wrapping, no `.ps1`/`.bat` references from app code.
- **External open:** no `explorer`/`xdg-open`/`open` invocations anywhere; external URLs go through `tauri_plugin_shell` (`shell:allow-open` capability), cross-platform by construction.
- **Watchdog fallback dir:** `std::env::temp_dir().join("mnemo-hang-reports")` — cross-platform (README's `%TEMP%` phrasing is Windows-flavored doc language, acceptable).

### 4. Build + bundle config

- `src-tauri/tauri.conf.json`: `bundle.targets: "all"` (darwin/dmg/app on macOS), `icon.icns` present, `macOS.minimumSystemVersion: "11.0"`. No updater config (nothing platform-specific to break). `beforeDevCommand`/`beforeBuildCommand` are plain `npm run` — cross-platform.
- `Cargo.toml` (lib) and `src-tauri/Cargo.toml`: `windows-sys` behind `[target.'cfg(windows)'.dependencies]` in both. No ungated Windows-only crates.
- `.cargo/config.toml`: rust-lld linker gated to `[target.x86_64-pc-windows-msvc]` with an explicit "macOS builds unaffected" comment. Correct.
- `frontend/package.json`: all scripts (vite/tsc/vitest/tauri) are shell-neutral.
- `.github/workflows/build.yml`: builds AND tests on `windows-latest` + `macos-latest`; macOS signing + notarization secrets-driven with fail-fast consistency checks. The README's "CI builds and tests on both Windows and macOS" claim is accurate.

### 5. Frontend platform checks

- No `navigator.platform`/`userAgent`/`userAgentData`/`process.platform` sniffing anywhere in frontend/src (exhaustive search).
- Path handling: normalize-then-split on `[/\\]` with dedicated Windows-path tests (`language.test.ts:37-38` `SRC\MAIN.RS`, `toolCardPaths` tests, `ProjectPicker.tsx:102`).
- BrowserView renders a red explanatory panel on unsupported platforms (macOS) instead of assuming WebView2.
- `mdEdit` tests cover CRLF content; `tauri.ts:516` references the WEBVIEW2 env var only in a doc comment.

### 6. Docs

- `README.md`: macOS build instructions present (macOS 11+, `npm install` → `npm run dev` / `npm run build` / `npx tauri build` from src-tauri); the Browser tab's Windows-only status is documented as an explicit exception; CI claim verified accurate. No incorrect "Windows-only" claims about neutral code.

---

## Out-of-axis observation (not counted, for the parent's triage)

`README.md:7` shows a "License: Apache 2.0" badge while `README.md:132` and `LICENSE` say MIT — a docs inconsistency unrelated to macOS readiness, flagged here only because it was noticed in passing.

## Summary table

| Severity | Title | Location |
|---|---|---|
| LOW | `npm start` wired to Windows-only `start.bat` | package.json:10 |
| LOW | Windows-only dev helper scripts, no macOS counterpart | start.bat, build.bat (root) |
| LOW | Sample config gives only the Windows copy target | debug.config.toml:4 |

**Bottom line:** 0 HIGH — nothing breaks the macOS build or app behavior. The three LOWs are dev-tooling/docs polish. The platform discipline in this codebase (gating, normalization, dual-platform CI, macOS-specific fixes like login-shell PATH adoption) is consistently and deliberately maintained.
