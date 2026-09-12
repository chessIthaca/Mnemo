// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The vitest include allow-list guard (live case 2027-01-07:
 * src/lib/tauri.test.ts was written, `npm test` reported green, and the
 * file never ran — the suite total stayed flat; the gap was caught only
 * by noticing the file missing from the run list. A SECOND live case
 * was caught by this very guard on its first run:
 * src/App.shellRender.test.ts had been silently skipped the same way).
 *
 * frontend/vitest.config.ts pins an EXPLICIT `include` allow-list (an
 * intentional curation, not vitest's default discovery glob), so a new
 * *.test.ts(x) file is invisible to the runner unless its path is
 * registered. Folklore ("remember to register") is not a guard: this
 * test makes the trap LOUD — it fails listing any test file under src/
 * that is not matched by `test.include`, so a green suite again proves
 * the whole suite ran.
 *
 * Self-reference caveat: if THIS file is removed from the include list,
 * the guard goes silent (it cannot run to complain). Don't.
 */

import { describe, expect, it } from "vitest";
import vitestConfig from "../../vitest.config.ts";

/**
 * Every *.test.{ts,tsx} under src/, discovered via Vite's own
 * transform-time glob (the same filesystem view a default discovery
 * include would use), as "src/…" paths matching the config's format.
 */
function discoverTestFiles(): string[] {
  return Object.keys(import.meta.glob("/src/**/*.test.{ts,tsx}"))
    .map((key) => key.replace(/^\//, ""))
    .sort();
}

/**
 * Convert an include entry (a glob or a literal path) to an anchored
 * RegExp. Handles the glob vocabulary the config uses: a double-star
 * followed by a slash → zero or more whole path segments (so a
 * settings-style double-star glob also matches files directly in that
 * directory), a bare double-star → anything, a single star → one
 * segment's worth, a question mark → one character. Everything else is
 * escaped, so literal entries match exactly themselves.
 */
function globToRegExp(glob: string): RegExp {
  let pattern = "^";
  for (let i = 0; i < glob.length; i++) {
    const ch = glob[i];
    if (ch === "*") {
      if (glob[i + 1] === "*" && glob[i + 2] === "/") {
        pattern += "(?:.*/)?"; // double-star + slash → zero or more segments
        i += 2;
      } else if (glob[i + 1] === "*") {
        pattern += ".*"; // bare double-star → anything
        i += 1;
      } else {
        pattern += "[^/]*"; // single star → one segment's worth
      }
    } else if (ch === "?") {
      pattern += "[^/]"; // question mark → one character
    } else if ("\\^$.|+()[]{}".includes(ch)) {
      pattern += `\\${ch}`; // escape regex specials
    } else {
      pattern += ch;
    }
  }
  return new RegExp(`${pattern}$`);
}

/**
 * The regression test body — named (rather than an inline `it` callback)
 * so the code graph and the plan's regression-test record can point at
 * it. Fails listing any test file under src/ that is not matched by
 * vitest.config.ts's `test.include` allow-list.
 */
function everyTestFileUnderSrcIsRegistered(): void {
  const include = vitestConfig.test?.include ?? [];
  expect(
    include.length,
    "vitest.config.ts must keep a non-empty test.include allow-list"
  ).toBeGreaterThan(0);

  const matchers = include.map((entry) => globToRegExp(entry));
  const unregistered = discoverTestFiles().filter(
    (file) => !matchers.some((re) => re.test(file))
  );

  expect(
    unregistered,
    "Unregistered test file(s) — vitest SILENTLY never runs them " +
      "(npm test stays green without executing them). Fix: add each " +
      "path to `test.include` in frontend/vitest.config.ts."
  ).toEqual([]);
}

describe("vitest include allow-list guard", () => {
  it(
    "every test file under src/ is registered in vitest.config.ts",
    everyTestFileUnderSrcIsRegistered
  );
});
