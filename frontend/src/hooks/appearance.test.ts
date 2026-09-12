// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Tests for the pure appearance helpers — currently the theme-aware accent
 * resolution (`effectiveAccentFor`), which keeps an uncustomized accent
 * readable in the light theme (cyan-400 is ≈1.8:1 on white) while a
 * customized accent applies as-is in both themes.
 */

import { describe, expect, it } from "vitest";
import {
  DEFAULT_ACCENT_COLOR,
  DEFAULT_ACCENT_COLOR_LIGHT,
  effectiveAccentFor,
} from "./appearance";

describe("effectiveAccentFor", () => {
  it("keeps the dark default in dark mode", () => {
    expect(effectiveAccentFor(DEFAULT_ACCENT_COLOR, false)).toBe(DEFAULT_ACCENT_COLOR);
  });

  it("swaps the dark default for the light accent in light mode", () => {
    // The unreadable-cyan-on-white fix: the light default is cyan-700
    // (≈5.4:1 on white).
    expect(effectiveAccentFor(DEFAULT_ACCENT_COLOR, true)).toBe(
      DEFAULT_ACCENT_COLOR_LIGHT,
    );
  });

  it("applies a customized accent as-is in both themes", () => {
    expect(effectiveAccentFor("#ff00ff", true)).toBe("#ff00ff");
    expect(effectiveAccentFor("#ff00ff", false)).toBe("#ff00ff");
  });

  it("treats the default case-insensitively", () => {
    expect(effectiveAccentFor("#22D3EE", true)).toBe(DEFAULT_ACCENT_COLOR_LIGHT);
  });
});
