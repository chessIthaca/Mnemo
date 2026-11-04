// Tests for scripts/rebuild-mnemo2.mjs. The quit command, the build and the
// logs are injected, so nothing here starts a helper or runs a build.

import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { MARKER_NAME, helperPath, markerPath, rebuild, resolveConfigDir } from "./rebuild-mnemo2.mjs";

/** A throwaway config dir, removed when the test ends. */
function tempConfig(t) {
  const dir = mkdtempSync(join(tmpdir(), "rebuild-mnemo2-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** Put a stand-in helper where the script expects the installed one. */
function installHelper(configDir) {
  mkdirSync(join(configDir, "bin"), { recursive: true });
  writeFileSync(helperPath(configDir), "");
}

/** Collect what the script logs and errors. */
function capture() {
  const stdout = [];
  const stderr = [];
  return {
    stdout,
    stderr,
    log: (line) => stdout.push(String(line)),
    error: (line) => stderr.push(String(line)),
  };
}

/** The marker as it is on disk right now, or undefined. */
function readMarker(configDir) {
  const path = markerPath(configDir);
  return existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : undefined;
}

test("the config dir follows $MNEMO_HOME, else ~/.mnemo", () => {
  assert.equal(resolveConfigDir({ MNEMO_HOME: join("some", "home") }), join("some", "home"));
  assert.equal(resolveConfigDir({}), join(homedir(), ".mnemo"));
});

test("a missing helper exits 1 without writing the marker or building", async (t) => {
  const configDir = tempConfig(t);
  const said = capture();
  const code = await rebuild({
    argv: [],
    configDir,
    runQuit: () => assert.fail("quit must not run without an installed helper"),
    runBuild: () => assert.fail("the build must not run without an installed helper"),
    log: said.log,
    error: said.error,
  });
  assert.equal(code, 1);
  assert.equal(readMarker(configDir), undefined);
  assert.deepEqual(said.stderr, [
    `rebuild:mnemo2: ${helperPath(configDir)} not found; run npm run build:mnemo2:native once`,
  ]);
});

test("a refused quit reports the helper's line, removes the marker and never builds", async (t) => {
  const configDir = tempConfig(t);
  installHelper(configDir);
  const said = capture();
  let built = false;
  const code = await rebuild({
    argv: [],
    configDir,
    runQuit: () => ({
      status: 1,
      stdout: "",
      stderr: "pid 1234: 2 worker(s) are running; rerun with --force to interrupt them\n",
    }),
    runBuild: () => {
      built = true;
      return 0;
    },
    log: said.log,
    error: said.error,
  });
  assert.equal(code, 1);
  assert.equal(built, false);
  assert.equal(readMarker(configDir), undefined);
  assert.deepEqual(said.stderr, [
    "rebuild:mnemo2: quit failed — pid 1234: 2 worker(s) are running; rerun with --force to interrupt them",
  ]);
});

test("a successful quit builds with the marker in place and removes it afterwards", async (t) => {
  const configDir = tempConfig(t);
  installHelper(configDir);
  const said = capture();
  const markerWhileQuitting = [];
  const markerWhileBuilding = [];
  let askedHelper;
  let askedArgs;
  const code = await rebuild({
    argv: [],
    configDir,
    runQuit: (helper, args) => {
      askedHelper = helper;
      askedArgs = args;
      markerWhileQuitting.push(readMarker(configDir));
      return { status: 0, stdout: "quit pid 1234 (C:\\Mnemo)\n", stderr: "" };
    },
    runBuild: () => {
      markerWhileBuilding.push(readMarker(configDir));
      return 0;
    },
    log: said.log,
    error: said.error,
  });
  assert.equal(code, 0);
  assert.equal(askedHelper, helperPath(configDir));
  assert.deepEqual(askedArgs, ["quit"]);
  assert.equal(markerWhileQuitting.length, 1);
  assert.equal(markerWhileQuitting[0].pid, process.pid);
  assert.equal(typeof markerWhileQuitting[0].started_ms, "number");
  assert.deepEqual(markerWhileBuilding, [markerWhileQuitting[0]]);
  assert.equal(readMarker(configDir), undefined);
  assert.deepEqual(said.stdout, [
    "quit pid 1234 (C:\\Mnemo)",
    "rebuild:mnemo2: done; the next Mnemo call or open_mission_control starts the new build",
  ]);
  assert.deepEqual(said.stderr, []);
});

test("a failed build returns its exit code and still removes the marker", async (t) => {
  const configDir = tempConfig(t);
  installHelper(configDir);
  const said = capture();
  const code = await rebuild({
    argv: [],
    configDir,
    runQuit: () => ({ status: 0, stdout: "", stderr: "" }),
    runBuild: () => 2,
    log: said.log,
    error: said.error,
  });
  assert.equal(code, 2);
  assert.equal(readMarker(configDir), undefined);
  assert.deepEqual(said.stdout, []);
  assert.deepEqual(said.stderr, []);
});

test("--force is passed through to the quit helper", async (t) => {
  const configDir = tempConfig(t);
  installHelper(configDir);
  const said = capture();
  let askedArgs;
  const code = await rebuild({
    argv: ["--force"],
    configDir,
    runQuit: (helper, args) => {
      askedArgs = args;
      return { status: 0, stdout: "", stderr: "" };
    },
    runBuild: () => 0,
    log: said.log,
    error: said.error,
  });
  assert.equal(code, 0);
  assert.deepEqual(askedArgs, ["quit", "--force"]);
});

test("a quit helper that cannot be started is reported as a quit failure", async (t) => {
  const configDir = tempConfig(t);
  installHelper(configDir);
  const said = capture();
  let built = false;
  const code = await rebuild({
    argv: [],
    configDir,
    runQuit: () => ({ status: 1, stdout: "", stderr: "", error: new Error("spawn EACCES") }),
    runBuild: () => {
      built = true;
      return 0;
    },
    log: said.log,
    error: said.error,
  });
  assert.equal(code, 1);
  assert.equal(built, false);
  assert.equal(readMarker(configDir), undefined);
  assert.deepEqual(said.stderr, ["rebuild:mnemo2: quit failed — spawn EACCES"]);
});

test("the marker lives in <config>/run/rebuilding", () => {
  assert.equal(markerPath(join("some", "home")), join("some", "home", "run", MARKER_NAME));
  assert.equal(MARKER_NAME, "rebuilding");
});
