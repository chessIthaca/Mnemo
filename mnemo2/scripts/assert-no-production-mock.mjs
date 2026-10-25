#!/usr/bin/env node
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Production must never import the test fixture layer.
 *
 * Phase 2 moved the seed projects/workers/lead entries into
 * `src/test/fixtures.ts`. That data is fine in tests and fatal in production:
 * it fabricates workers, checks, and merges that no engine ever produced.
 *
 * This check walks the production trees only (`src/lib`, `src/components`,
 * `src/App.svelte`, `src/main.ts`), so a test file re-importing the fixtures is
 * never a finding.
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));

/** Directories and files that make up the shipped bundle. */
const PRODUCTION_TARGETS = [
  "src/lib",
  "src/components",
  "src/App.svelte",
  "src/main.ts",
];

/** Import specifiers that would pull fixture data into the bundle. */
const FORBIDDEN = [/test\/fixtures/, /\.\/fixtures/, /\.\.\/test\//];

function walk(target) {
  const absolute = join(root, target);
  let stats;
  try {
    stats = statSync(absolute);
  } catch {
    return [];
  }
  if (stats.isFile()) return [absolute];
  const files = [];
  for (const entry of readdirSync(absolute, { withFileTypes: true })) {
    const child = join(absolute, entry.name);
    if (entry.isDirectory()) {
      files.push(...walk(relative(root, child)));
    } else if (/\.(ts|svelte)$/.test(entry.name)) {
      files.push(child);
    }
  }
  return files;
}

const findings = [];
for (const target of PRODUCTION_TARGETS) {
  for (const file of walk(target)) {
    if (/\.test\.ts$/.test(file)) continue;
    const text = readFileSync(file, "utf8");
    for (const pattern of FORBIDDEN) {
      if (!pattern.test(text)) continue;
      const line = text
        .split(/\r?\n/)
        .findIndex((candidate) => pattern.test(candidate));
      findings.push(`${relative(root, file)}:${line + 1} imports test fixtures`);
      break;
    }
  }
}

if (findings.length > 0) {
  console.error("Production imports test fixtures:");
  for (const finding of findings) console.error(`  ${finding}`);
  console.error(
    "\nMove the data into the component/store or delete it; fixtures are test-only.",
  );
  process.exit(1);
}

console.log("No production file imports test fixtures.");
