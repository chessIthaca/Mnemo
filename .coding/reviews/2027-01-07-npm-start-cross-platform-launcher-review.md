## Verdict: PASS

Review of plan 55b6e556 — "Cross-platform npm start launcher — scripts/start.mjs (mac-readiness LOW 1+2)" — over ALL uncommitted changes on wt/agenticcoding: scripts/start.mjs (NEW, 124 lines), package.json (start line rewired), README.md (Building section, both platforms), plus expected bookkeeping (.coding/backlog.jsonl status flip, .coding/plans/55b6e556.md untracked plan file).

Zero findings. The launcher is a faithful, correct cross-platform port of the start.bat/build.bat workflow knowledge; the spawn mechanics are sound on both platforms; the docs are accurate and in sync; nothing else in the repo referenced `npm start` → start.bat and went stale. Detailed verification below.

## Scope reviewed

- `git diff HEAD` + `git status --short`: README.md (+2 lines net, Building section), package.json (1 line), .coding/backlog.jsonl (bookkeeping: item 263f58a5 pending → in_flight, plan_id 55b6e556), untracked scripts/start.mjs + .coding/plans/55b6e556.md.
- Read in full: scripts/start.mjs, start.bat, build.bat, package.json, frontend/package.json, src-tauri/tauri.conf.json, src-tauri/Cargo.toml, root Cargo.toml, README.md:85-137, .github/workflows/build.yml, node_modules/@tauri-apps/cli/package.json.
- Tree-walk search for `start.bat` (38 matches / 15 files — all reviewed) and `npm start` (historical .coding records only; the README's own mentions verified by direct read since the content index served a stale pre-change README).

## 1. Faithful encoding of the .bat workflow knowledge — VERIFIED

- **npm-sync-first**: start.mjs:67-81 runs `npm install` before any branch that invokes the CLI, with the rationale comment (dependency added after last install → frontend `tsc` dies with "Cannot find module") carried over nearly verbatim from start.bat:5-16. Failure → friendly message + exit 1 (start.mjs:79-80 ≈ start.bat:12-16, minus the double-click `pause` — correct, since npm context doesn't need it).
- **src-tauri cwd**: all three CLI invocations pass `cwd: srcTauri` (start.mjs:91, 96, 109) — matching start.bat:27/40-43 and build.bat:13-15. Empirically confirmed by the verification run (tauri read src-tauri/tauri.conf.json — the bundle-identifier warning proves it).
- **--no-bundle default vs bundle opt-in**: start.mjs:109 (`build --no-bundle`) vs :96 (bundle → plain `build`) — exactly start.bat:41 and build.bat:16-22.
- **Exit-code plumbing**: `run()` (start.mjs:47-54) returns `result.status ?? 1` (signal-kill → 1), spawn errors print + exit 1, and every branch ends in `process.exit(<child status>)` — strictly better than the .bat's `TAURI_EXIT` dance (the app's own exit code now propagates to `npm start`, which start.bat:51 never did).
- **Windows taskkill guard**: start.mjs:104-106 — `taskkill /IM mnemo-app.exe`, no `/F` (WM_CLOSE graceful close, matching the .bat's documented intent), stdio ignored, result deliberately ignored (taskkill fails when no instance runs — the common case), default mode only. Faithful port of start.bat:33-37.
- **CLI existence check**: start.mjs:85-88 checks the exact file it invokes (`node_modules/@tauri-apps/cli/tauri.js`) rather than the .bat's `node_modules\.bin\tauri.cmd` shim — the correct adaptation, and it sits above the dev branch (the ipc#4 fix from the .bat's history, right from birth here).

## 2. Cross-platform spawn mechanics — VERIFIED

- **node→tauri.js**: `run(process.execPath, [cliJs, ...])` (start.mjs:91, 96, 109) — pure node→JS spawn, no .cmd/.sh shim. Confirmed `bin: { "tauri": "./tauri.js" }` in node_modules/@tauri-apps/cli/package.json:52-54 (v2.11.4). Identical on every platform.
- **cmd.exe /c npm on Windows**: start.mjs:73-77. The comment accurately states the CVE-2024-27980 rationale (Node ≥ 18.20/20.12 refuses to spawn .cmd/.bat directly — EINVAL), matching the empirically confirmed probe on this machine's Node v24.7.0. `cmd.exe` is a real PE resolved from PATH; `taskkill` likewise (start.mjs:105) — neither is a .cmd shim, so both are unaffected by the restriction. Plain `npm` on Unix (start.mjs:76-77) — npm's Unix bin is a shebang script, which execvp handles.
- **Built binary launch**: start.mjs:123-124 spawns `root/target/release/mnemo-app(.exe)` — a real PE/Mach-O, not a shim. `cwd: root` matches start.bat:51 (run from root after popd).

## 3. Binary path = workspace-root target — VERIFIED (three independent sources)

- Root Cargo.toml:149-150: `[workspace] members = ["src-tauri"]` — the workspace root IS the repo root, so cargo artifacts land in `root/target/`.
- src-tauri/Cargo.toml:2: package name `mnemo-app` → binary `mnemo-app` / `mnemo-app.exe`.
- start.bat:51 (`target\release\mnemo-app.exe` from root) and the CI workflow's own note (.github/workflows/build.yml:65-67: "the workspace root is the REPO ROOT … all bundles land under <repo-root>/target/").
- start.mjs:123 `path.join(root, 'target', 'release', …)` — correct. The `bundle` mode's "installers under target/release/bundle" (USAGE :44, README:97) matches tauri's layout and README:106.

## 4. Multi-platform neutrality — VERIFIED

`isWindows` guards exactly three concerns: the npm invocation (start.mjs:76-77), the taskkill (start.mjs:104), and the exe suffix (start.mjs:117, 123). Everything else is platform-neutral: `path.join` throughout (no hardcoded separators), root derived from `import.meta.url` (not cwd — robust from any invocation directory), no kill on Unix (correct: Unix replaces running binaries), `mnemo-app` unsuffixed on macOS. No Windows-only assumptions leak outside the guards. This satisfies the project's multi-platform-neutrality review expectation.

## 5. package.json + README accuracy — VERIFIED

- package.json: valid JSON; the diff touches only line 10 (`"start": "node scripts/start.mjs"`); dev/build entries unchanged.
- README:97 quick-start describes the launcher's actual behavior (sync → build --no-bundle → launch; dev/build/bundle modes). The `:5179` Vite port matches tauri.conf.json:8 (`devUrl`). "start.bat/build.bat remain as Windows-native conveniences" — accurate and documents the drift risk.
- README:115 (macOS): "`npm start` (and its dev/build/bundle modes) works identically to Windows" — accurate; the manual equivalent and the `.app`/`.dmg` bundle note are correct (tauri.conf.json bundle.targets "all").

## 6. Doc sync — no stale references — VERIFIED

Tree-walk search for `start.bat` (38 matches, 15 files): README:97 (accurate convenience mention), scripts/start.mjs:3,5 (historical context comments), src-tauri/src/console.rs:1991 (historical E0004 bug note — not a wiring reference), start.bat itself, and .coding/* historical records (plans/reviews/backlog — correct to leave). Nothing anywhere still claims `npm start` runs start.bat. CI (.github/workflows/build.yml) uses `npm ci` + `npx tauri build` directly — it never invokes `npm start`, so CI behavior is unaffected.

## 7. Security — VERIFIED

Every spawn uses a fixed argv array with `shell` unset (no `shell:true`): `['/c', 'npm', 'install']`, `['/IM', 'mnemo-app.exe']`, `[cliJs, 'dev'|'build'|'build', '--no-bundle']`, `[]`. The only user input (`process.argv[2]`) is validated against the whitelist `['', 'dev', 'build', 'bundle']` (start.mjs:62) before any command runs and is never interpolated into a command line. The cmd.exe invocation passes args as separate argv elements with no spaces/metacharacters — no quoting or cmd-injection surface.

## 8. Keeping the .bats — REASONABLE

start.bat/build.bat unchanged (diff confirms). They serve a different UX (double-click + `pause` on failure); the README now names them as Windows-native conveniences, and the drift risk is accepted and documented rather than hidden. Removing them in an unattended run would have been the riskier call. Strict mode validation (typo → usage error, exit 1, start.mjs:62-65) vs the .bat's lenient fall-through is a deliberate, documented improvement — a typo must not trigger a full release build + app launch.

## Notes (informational, not findings)

- **taskkill WM_CLOSE race** (start.mjs:104-106): taskkill returns as soon as WM_CLOSE is posted; a very slow app shutdown could still race the build. This is byte-for-byte the pre-existing start.bat:37 behavior, deliberately preserved — not introduced by this change.
- **bundle/build modes don't kill a running instance on Windows** (cargo "Access is denied" if the app is running): faithful port of build.bat's no-kill precedent, documented as design decision (4).
- The unattended verification correctly skipped the default mode's final app-launch step (it would have killed/spawned a second instance); the error path and exit-code plumbing were instead validated by the real locked-exe build failure ("Build failed." + exit 1).
- No Rust/TS source changed, so the green suites (root 2008+16, src-tauri 216+4, 0 failed, zero warnings) plus the full `node scripts/start.mjs build` release run constitute complete verification for this delta.
