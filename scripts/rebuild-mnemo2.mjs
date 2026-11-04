// One-command rebuild of Mnemo2: ask every running Mnemo2 to quit, keep the
// `mnemo2-mcp` helper from starting one mid-build, then build and install.
//
// A running `mnemo2.exe` locks `target/release/mnemo2.exe` on Windows, so the
// release build fails with "Access is denied". Closing the window only hides
// it to the tray, and the next Mnemo tool call (or hook) makes the helper
// start a hidden Mnemo2 again. So `mnemo2-mcp quit` asks every instance that
// published a run file to leave, and `<config>/run/rebuilding` tells the
// helper that a spawn during the build has to wait.
//
// An instance that refuses, does not answer, or does not exit in time stops
// the script before the build with one plain line and exit code 1: nothing is
// ever killed.
//
// Usage: npm run rebuild:mnemo2 [-- --force]

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/** The marker `instance.rs` `rebuild_in_progress` reads. */
export const MARKER_NAME = "rebuilding";

/** The native build this script wraps (package.json `build:mnemo2:native`). */
const BUILD_COMMAND = "npm run build:mnemo2:native";

const EXE = process.platform === "win32" ? ".exe" : "";

/** `<config>`: $MNEMO_HOME, else `~/.mnemo` (the helper's own rule). */
export function resolveConfigDir(env = process.env) {
  return env.MNEMO_HOME || join(homedir(), ".mnemo");
}

/** The installed helper whose `quit` command asks the instances to leave. */
export function helperPath(configDir) {
  return join(configDir, "bin", `mnemo2-mcp${EXE}`);
}

/** The marker that tells the helper a rebuild is in progress. */
export function markerPath(configDir) {
  return join(configDir, "run", MARKER_NAME);
}

/**
 * Ask the installed helper to quit every instance: `mnemo2-mcp quit [--force]`.
 * Resolves with `{ status, stdout, stderr }` (`error` when the helper could
 * not be started), so the caller can report the helper's own line.
 */
function quitHelper(helper, args) {
  return new Promise((done) => {
    const child = spawn(helper, args, {
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", (error) => done({ status: 1, stdout, stderr, error }));
    child.on("close", (code, signal) => {
      done({ status: code ?? (signal ? 1 : 0), stdout, stderr });
    });
  });
}

/**
 * Run the native build with inherited stdio. Resolves with its exit code (1
 * when it could not even start).
 */
function buildNative() {
  return new Promise((done) => {
    const child = spawn(BUILD_COMMAND, { stdio: "inherit", shell: true });
    child.on("error", (error) => {
      console.error(`rebuild:mnemo2: could not run ${BUILD_COMMAND}: ${error.message}`);
      done(1);
    });
    child.on("close", (code, signal) => {
      done(code ?? (signal ? 1 : 0));
    });
  });
}

/** The line a failed quit carries: the helper's own stderr, else what failed. */
function quitReason(result) {
  const stderr = typeof result.stderr === "string" ? result.stderr.trim() : "";
  if (stderr) return stderr;
  if (result.error) return result.error.message ?? String(result.error);
  return `mnemo2-mcp quit exited with code ${result.status ?? "unknown"}`;
}

/**
 * One rebuild: refuse when the helper is missing, write the marker, ask every
 * running Mnemo2 to quit, build, then remove the marker.
 *
 * Every side effect is injected for the tests: `runQuit(helper, args)`
 * resolves with `{ status, stdout, stderr }` (or throws), `runBuild()` with
 * the build's exit code and prints its own output. Returns the process exit
 * code: 0 only when the build succeeded. Nothing here kills a process.
 */
export async function rebuild({
  argv = process.argv.slice(2),
  configDir = resolveConfigDir(),
  runQuit = quitHelper,
  runBuild = buildNative,
  log = console.log,
  error = console.error,
} = {}) {
  const helper = helperPath(configDir);
  if (!existsSync(helper)) {
    error(`rebuild:mnemo2: ${helper} not found; run npm run build:mnemo2:native once`);
    return 1;
  }
  const marker = markerPath(configDir);
  try {
    mkdirSync(join(configDir, "run"), { recursive: true });
    writeFileSync(marker, `${JSON.stringify({ pid: process.pid, started_ms: Date.now() })}\n`);
  } catch (failure) {
    error(`rebuild:mnemo2: could not write ${marker}: ${failure.message}`);
    return 1;
  }
  const args = argv.includes("--force") ? ["quit", "--force"] : ["quit"];
  try {
    let quit;
    try {
      quit = await runQuit(helper, args);
    } catch (failure) {
      error(`rebuild:mnemo2: quit failed — ${failure?.message ?? failure}`);
      return 1;
    }
    const said = typeof quit.stdout === "string" ? quit.stdout.trimEnd() : "";
    if (said) log(said);
    if (quit.status !== 0) {
      error(`rebuild:mnemo2: quit failed — ${quitReason(quit)}`);
      return 1;
    }
    const status = await runBuild();
    if (status !== 0) return Number.isInteger(status) ? status : 1;
    log("rebuild:mnemo2: done; the next Mnemo call or open_mission_control starts the new build");
    return 0;
  } finally {
    rmSync(marker, { force: true });
  }
}

/** True when `node` was started with this file rather than importing it. */
function invokedDirectly() {
  if (process.argv[1] === undefined) return false;
  const self = fileURLToPath(import.meta.url);
  const entry = resolve(process.argv[1]);
  return process.platform === "win32"
    ? entry.toLowerCase() === self.toLowerCase()
    : entry === self;
}

if (invokedDirectly()) {
  const configDir = resolveConfigDir();
  // A Ctrl+C during the build must not leave the helper blocked: drop the
  // marker before exiting. `rebuild`'s `finally` covers every other path.
  const stop = (code) => () => {
    rmSync(markerPath(configDir), { force: true });
    process.exit(code);
  };
  process.once("SIGINT", stop(130));
  process.once("SIGTERM", stop(143));
  process.exitCode = await rebuild({ argv: process.argv.slice(2), configDir });
}
