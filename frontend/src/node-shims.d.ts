// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Minimal ambient module shims for Node builtins used by tests. This project
 * deliberately doesn't install @types/node (the app code is pure browser;
 * only vitest tests run in Node), so a test file that touches a builtin
 * declares just the surface it needs here. Currently used by
 * src/styles/scrollbar.test.ts, which reads globals.css from disk for its
 * CSS-contract assertions (vitest stubs `.css` imports to an empty string,
 * so `?raw`/`?inline` can't be used).
 */
declare module "node:fs" {
  export function readFileSync(path: URL, encoding: "utf8"): string;
}
