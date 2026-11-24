// Keep the temp folders the JavaScript tests create inside the checkout and on
// its drive (plan 35105099). Node's os.tmpdir() reads TMPDIR on Unix and
// TEMP/TMP on Windows on every call, so setting them here redirects every
// mkdtempSync(join(tmpdir(), …)) and every child process the tests spawn —
// nothing lands in the user's system temp folder. `target/tmp` is cargo's own
// CARGO_TARGET_TMPDIR, so `cargo clean` and the pool trim remove it with the
// checkout.
//
// This is plain JavaScript (types in test-tmp.d.mts) because mnemo2's TypeScript
// program has no Node types: vitest.config.ts and the vitest guard import it.
//
// Usage: a scripts/*.test.mjs calls testTmpRoot() once at module top level,
// before it creates a temp dir or spawns a child.

import { existsSync, mkdirSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, sep } from "node:path";
import { fileURLToPath } from "node:url";

/** Compare two paths the way the platform does: case-insensitively on Windows. */
function fold(path) {
  return process.platform === "win32" ? path.toLowerCase() : path;
}

/**
 * The checkout's own test temp root, `<repo root>/target/tmp`: created if it is
 * missing and exported to this process and its children as TMPDIR, TEMP and
 * TMP, so os.tmpdir() follows it.
 *
 * @returns {string} the absolute path of the folder.
 */
export function testTmpRoot() {
  const root = fileURLToPath(new URL("../target/tmp", import.meta.url));
  mkdirSync(root, { recursive: true });
  process.env.TMPDIR = root;
  process.env.TEMP = root;
  process.env.TMP = root;
  return root;
}

/**
 * The checkout root holding `moduleUrl`: the nearest folder above it with a
 * `scripts/test-tmp.mjs`, canonicalized with realpath. Resolving it from the
 * caller's own file keeps the guard independent of this module's location.
 *
 * @param {string} moduleUrl a `file:` URL of a module inside the checkout
 *   (typically the calling test's `import.meta.url`).
 * @returns {string} the absolute path of the checkout root.
 */
export function checkoutRootOf(moduleUrl) {
  const modulePath = fileURLToPath(moduleUrl);
  let dir = dirname(modulePath);
  for (;;) {
    if (existsSync(join(dir, "scripts", "test-tmp.mjs"))) {
      return realpathSync(dir);
    }
    const parent = dirname(dir);
    if (parent === dir) {
      throw new Error(`no checkout root above ${modulePath}`);
    }
    dir = parent;
  }
}

/**
 * What `os.tmpdir()` reports right now, canonicalized with realpath — the folder
 * the test's temp dirs actually land in.
 *
 * @returns {string} the absolute path of the process's temp folder.
 */
export function tmpdirNow() {
  return realpathSync(tmpdir());
}

/**
 * The checkout's test temp folder, `<checkout root>/target/tmp`, worked out from
 * `moduleUrl` — the folder a redirected `os.tmpdir()` must equal.
 *
 * @param {string} moduleUrl a `file:` URL of a module inside the checkout.
 * @returns {string} the absolute path of the checkout's test temp folder.
 */
export function targetTmpOf(moduleUrl) {
  return join(checkoutRootOf(moduleUrl), "target", "tmp");
}

/**
 * Whether two paths name the same folder.
 *
 * @param {string} a one absolute path.
 * @param {string} b another absolute path.
 * @returns {boolean} true when they are the same folder, case-insensitively on
 *   Windows.
 */
export function samePath(a, b) {
  return fold(a) === fold(b);
}

/**
 * Whether `child` lies inside `parent` (and is not `parent` itself).
 *
 * @param {string} child the path expected below `parent`.
 * @param {string} parent the folder expected to contain `child`.
 * @returns {boolean} true when `child` is strictly inside `parent`,
 *   case-insensitively on Windows.
 */
export function isInside(child, parent) {
  const root = parent.endsWith(sep) ? parent : `${parent}${sep}`;
  return fold(child).startsWith(fold(root));
}
