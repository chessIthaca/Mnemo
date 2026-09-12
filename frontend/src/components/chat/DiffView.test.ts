// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect } from "vitest";
import {
  computeDiff,
  parseUnifiedDiff,
  LCS_CELL_BUDGET,
} from "./DiffView";

describe("parseUnifiedDiff", () => {
  it("classifies headers, adds, removes, and context", () => {
    const text = [
      "--- a.txt",
      "+++ a.txt",
      "@@ -1,2 +1,2 @@",
      " hello",
      "-world",
      "+rust",
    ].join("\n");
    const lines = parseUnifiedDiff(text);
    expect(lines.map((l) => l.type)).toEqual([
      "meta",
      "meta",
      "meta",
      "context",
      "remove",
      "add",
    ]);
    expect(lines[4].text).toBe("world");
    expect(lines[5].text).toBe("rust");
  });

  it("does not treat dashed content as a header without space", () => {
    // A remove line for content "---not a header" starts with "----" after
    // the marker is stripped only for +/-; the raw line is "----not…".
    const lines = parseUnifiedDiff("-not-header\n+ok\n");
    expect(lines[0]).toEqual({ type: "remove", text: "not-header" });
    expect(lines[1]).toEqual({ type: "add", text: "ok" });
  });
});

describe("computeDiff", () => {
  it("marks simple replacements", () => {
    const lines = computeDiff("a\nb\nc", "a\nB\nc");
    expect(lines).toEqual([
      { type: "context", text: "a" },
      { type: "remove", text: "b" },
      { type: "add", text: "B" },
      { type: "context", text: "c" },
    ]);
  });

  it("skips LCS when n*m exceeds the cell budget", () => {
    // Choose n,m so n*m > budget but the strings stay small enough to build.
    // Use sqrt(~budget)+1 on each side.
    const side = Math.floor(Math.sqrt(LCS_CELL_BUDGET)) + 2;
    const oldText = Array.from({ length: side }, (_, i) => `o${i}`).join("\n");
    const newText = Array.from({ length: side }, (_, i) => `n${i}`).join("\n");
    const lines = computeDiff(oldText, newText);
    expect(lines[0].type).toBe("meta");
    expect(lines[0].text).toMatch(/truncated/);
    const removes = lines.filter((l) => l.type === "remove");
    const adds = lines.filter((l) => l.type === "add");
    // Fallback caps painted lines (LCS_FALLBACK_MAX_LINES / 2 each side).
    expect(removes.length).toBeLessThanOrEqual(side);
    expect(adds.length).toBeLessThanOrEqual(side);
    expect(removes.length + adds.length).toBeLessThanOrEqual(4_000);
  });
});
