// Tests for scripts/install-mnemo2-mcp.mjs: the refresh of an installed plugin
// copy (plan 0fa5cad0). Each test runs the script as a child process inside a
// throwaway sandbox — its own checkout root, CARGO_TARGET_DIR, HOME and plugin
// registry — so no real build, plugin cache, ~/.mnemo or repo plugin bin is
// touched, and a fake "helper" stands in for the freshly built one.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const SCRIPT = fileURLToPath(new URL("./install-mnemo2-mcp.mjs", import.meta.url));
const EXE = process.platform === "win32" ? ".exe" : "";

/** Write `content` to `path`, creating its folder first. */
function write(path, content) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, content);
}

/**
 * A throwaway checkout, removed when the test ends: the script under test (a
 * byte-for-byte copy, so the script's `root` is the sandbox), the two built
 * binaries it looks for, and the plugin source folder it refreshes from.
 */
function sandbox(t) {
  const dir = mkdtempSync(join(tmpdir(), "install-mnemo2-mcp-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  write(
    join(dir, "scripts", "install-mnemo2-mcp.mjs"),
    readFileSync(SCRIPT, "utf8"),
  );
  const release = join(dir, "target", "release");
  write(join(release, `mnemo2-mcp${EXE}`), "helper");
  write(join(release, `mnemo-models${EXE}`), "models");
  write(join(release, `mnemo2${EXE}`), "app");
  write(join(dir, "mnemo2", "plugin", "hooks", "hooks.json"), '{"hooks":{}}');
  return { dir, release, home: join(dir, "home") };
}

/** Register an installed plugin copy at `target` in `home`'s registry. */
function register(home, target) {
  write(
    join(home, ".claude", "plugins", "installed_plugins.json"),
    JSON.stringify({
      version: 2,
      plugins: { "mnemo@mnemo": [{ scope: "user", installPath: target }] },
    }),
  );
}

/** The sandbox's installed-copy folder, with `bin` as a folder holding `helper`. */
function installedCopy(box, helper) {
  const cache = join(box.home, "cache", "mnemo", "mnemo", "2.5.0");
  write(join(cache, "bin", `mnemo2-mcp${EXE}`), helper);
  write(join(cache, "hooks", "hooks.json"), "{}");
  register(box.home, cache);
  return cache;
}

/** Run the sandboxed script; HOME and USERPROFILE both point into the sandbox. */
function run(box) {
  const env = {
    ...process.env,
    CARGO_TARGET_DIR: join(box.dir, "target"),
    HOME: box.home,
    USERPROFILE: box.home,
  };
  delete env.MNEMO_HOME;
  return spawnSync(process.execPath, [join(box.dir, "scripts", "install-mnemo2-mcp.mjs")], {
    encoding: "utf8",
    env,
  });
}

test("refresh_installs_the_helper_into_the_installed_copy", (t) => {
  const box = sandbox(t);
  const cache = installedCopy(box, "helper-from-november-16");

  const result = run(box);

  assert.equal(result.status, 0, result.stderr);
  assert.equal(
    readFileSync(join(cache, "bin", `mnemo2-mcp${EXE}`), "utf8"),
    "helper",
    "the installed copy must run the freshly built helper, not the one it was installed with",
  );
  assert.equal(
    readFileSync(join(cache, "bin", "mnemo2-path.txt"), "utf8"),
    `${join(box.release, `mnemo2${EXE}`)}\n`,
    "mnemo2-path.txt beside the refreshed helper names the matching app",
  );
  assert.equal(
    readFileSync(join(cache, "hooks", "hooks.json"), "utf8"),
    '{"hooks":{}}',
    "the hooks, skills and manifest refresh still runs",
  );
  assert.match(result.stdout, /refreshed the installed plugin copy/);
});

test("a_failing_installed_copy_never_fails_the_install", (t) => {
  const box = sandbox(t);
  const cache = join(box.home, "cache", "mnemo", "mnemo", "2.5.0");
  write(join(cache, "bin"), "not a folder");
  register(box.home, cache);

  const result = run(box);

  assert.equal(result.status, 0, "a cache copy that cannot be refreshed is best effort");
  assert.match(result.stderr, /could not refresh/);
  assert.equal(readFileSync(join(cache, "bin"), "utf8"), "not a folder");
});

test("a_missing_registry_is_skipped", (t) => {
  const box = sandbox(t);

  const result = run(box);

  assert.equal(result.status, 0, result.stderr);
  assert.doesNotMatch(result.stdout, /refreshed the installed plugin copy/);
});
