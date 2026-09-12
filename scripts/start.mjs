// Cross-platform dev/build launcher for Mnemo (mac-readiness review LOW 1+2).
//
// `npm start` used to be wired to start.bat — a batch file sh can't run, so
// a macOS developer's most instinctive entry point failed. This launcher
// encodes the same workflow knowledge as start.bat/build.bat so one command
// works identically on Windows and macOS:
//
// - npm install runs FIRST (before any branch that invokes the Tauri CLI):
//   a dependency added to package.json after the last install is otherwise
//   missing at build time and the frontend `tsc` step dies with
//   "Cannot find module ...". npm install is a fast no-op when in sync.
// - The Tauri CLI is hoisted to the root workspace node_modules and must
//   run with src-tauri/ as the working directory, or tauri.conf.json is
//   not detected. It is invoked as `node <cli>/tauri.js` — no .cmd/.sh
//   shim, so the spawn is identical on every platform.
// - Default is `tauri build --no-bundle` (fast local build, no installer);
//   `bundle` opts into installers.
//
// Modes:
//   (no arg)   build --no-bundle, then launch the app
//   dev        tauri dev (Vite dev server on :5179 + native window)
//   build      build --no-bundle, do not launch
//   bundle     tauri build (installers under target/release/bundle)
//
// Windows-only detail: a running instance locks the app exe and makes the
// build fail with "Access is denied" — the default mode closes it first
// (the script relaunches the app at the end anyway). Unix allows replacing
// a running binary, so no kill is needed on macOS.

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const cliJs = path.join(root, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');
const srcTauri = path.join(root, 'src-tauri');
const isWindows = process.platform === 'win32';

const USAGE = `Usage: npm start [--] [dev|build|bundle]
  (no arg)  build --no-bundle, then launch the app
  dev       tauri dev (Vite dev server + native window)
  build     build --no-bundle, do not launch
  bundle    tauri build (installers under target/release/bundle)`;

/** Run a command, inherit stdio, return its exit code (1 on spawn error). */
function run(cmd, args, opts = {}) {
    const result = spawnSync(cmd, args, { stdio: 'inherit', ...opts });
    if (result.error) {
        console.error(`mnemo: failed to run ${cmd}: ${result.error.message}`);
        process.exit(1);
    }
    return result.status ?? 1;
}

const mode = process.argv[2] ?? '';

if (mode === '--help' || mode === '-h') {
    console.log(USAGE);
    process.exit(0);
}
if (!['', 'dev', 'build', 'bundle'].includes(mode)) {
    console.error(`Unknown mode: ${mode}\n${USAGE}`);
    process.exit(1);
}

// Sync node_modules with package.json BEFORE any branch that invokes the
// CLI. Without this, a dependency added to package.json after the last
// install is missing at build time and the frontend `tsc` step dies with
// "Cannot find module ...". npm install is a fast no-op when already in
// sync.
console.log('Syncing npm dependencies...');
// Node (≥ 18.20 / 20.12 — CVE-2024-27980) refuses to spawn .cmd/.bat files
// directly (EINVAL), so on Windows npm is invoked through cmd.exe; on
// macOS plain `npm` is fine.
const npmCmd = isWindows ? 'cmd.exe' : 'npm';
const npmArgs = isWindows ? ['/c', 'npm', 'install'] : ['install'];
if (run(npmCmd, npmArgs, { cwd: root }) !== 0) {
    console.error('npm install failed. Fix the errors above and run npm start again.');
    process.exit(1);
}

// `npm start dev` must fail with a friendly message (not a raw "not found")
// if the CLI still is not there after the install.
if (!fs.existsSync(cliJs)) {
    console.error('Tauri CLI not found after npm install. Check package.json.');
    process.exit(1);
}

if (mode === 'dev') {
    process.exit(run(process.execPath, [cliJs, 'dev'], { cwd: srcTauri }));
}

if (mode === 'bundle') {
    console.log('Building frontend + Rust binary + installers...');
    process.exit(run(process.execPath, [cliJs, 'build'], { cwd: srcTauri }));
}

// Windows: a running instance locks the app exe (the release build) and
// makes the build fail with "Access is denied". Close it gracefully
// (WM_CLOSE; the app's exit hook shuts down cleanly) before rebuilding —
// the default mode relaunches the app at the end anyway. Unix allows
// replacing a running binary, so no kill is needed on macOS.
if (mode === '' && isWindows) {
    spawnSync('taskkill', ['/IM', 'mnemo-app.exe'], { stdio: 'ignore' });
}

console.log('Building frontend + Rust binary, no installer...');
const buildExit = run(process.execPath, [cliJs, 'build', '--no-bundle'], { cwd: srcTauri });
if (buildExit !== 0) {
    console.error('Build failed.');
    process.exit(buildExit);
}

if (mode === 'build') {
    console.log(
        `Build complete: ${path.join('target', 'release', isWindows ? 'mnemo-app.exe' : 'mnemo-app')}`
    );
    process.exit(0);
}

console.log('Starting Mnemo...');
const bin = path.join(root, 'target', 'release', isWindows ? 'mnemo-app.exe' : 'mnemo-app');
process.exit(run(bin, [], { cwd: root }));
