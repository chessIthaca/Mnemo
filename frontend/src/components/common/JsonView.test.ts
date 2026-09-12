// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect } from "vitest";
import { summarize, shouldAutoExpand } from "./JsonView";

describe("summarize", () => {
  it("counts object keys with singular/plural", () => {
    expect(summarize({})).toBe("{0 keys}");
    expect(summarize({ a: 1 })).toBe("{1 key}");
    expect(summarize({ a: 1, b: 2, c: 3 })).toBe("{3 keys}");
  });

  it("counts array items with singular/plural", () => {
    expect(summarize([])).toBe("[0 items]");
    expect(summarize([1])).toBe("[1 item]");
    expect(summarize([1, 2, 3, 4])).toBe("[4 items]");
  });

  it("returns empty string for leaf values", () => {
    expect(summarize(null)).toBe("");
    expect(summarize(undefined)).toBe("");
    expect(summarize("hello")).toBe("");
    expect(summarize(42)).toBe("");
    expect(summarize(true)).toBe("");
  });

  it("treats nested containers by their own child count", () => {
    expect(summarize({ a: [1, 2], b: { c: 3 } })).toBe("{2 keys}");
    expect(summarize([[1, 2], { a: 1 }])).toBe("[2 items]");
  });
});

describe("shouldAutoExpand", () => {
  it("expands depths strictly below defaultDepth", () => {
    // depth 0 is the root; defaultDepth 1 means only the root opens.
    expect(shouldAutoExpand(0, 1)).toBe(true);
    expect(shouldAutoExpand(1, 1)).toBe(false);
    expect(shouldAutoExpand(0, 2)).toBe(true);
    expect(shouldAutoExpand(1, 2)).toBe(true);
    expect(shouldAutoExpand(2, 2)).toBe(false);
  });

  it("defaultDepth 0 collapses even the root", () => {
    expect(shouldAutoExpand(0, 0)).toBe(false);
  });
});
