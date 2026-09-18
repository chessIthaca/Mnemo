// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Tests for the pure appearance helpers — the theme-aware accent
 * resolution (`effectiveAccentFor`), which keeps an uncustomized accent
 * readable in the light theme (cyan-400 is ≈1.8:1 on white) while a
 * customized accent applies as-is in both themes, and the right-panel
 * width fraction band + legacy-px seed (plan 38b0e2eb).
 */

import { describe, expect, it, vi } from "vitest";
import {
  DEFAULT_ACCENT_COLOR,
  DEFAULT_ACCENT_COLOR_LIGHT,
  clampPanelFraction,
  effectiveAccentFor,
  readRightPanelWidthFrac,
  readShowShellPreview,
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

describe("clampPanelFraction", () => {
  it("keeps an in-band fraction unchanged", () => {
    expect(clampPanelFraction(0.4, 1000)).toBe(0.4);
  });

  it("clamps a too-wide fraction to the chat-column minimum", () => {
    // The reporting box's real case: 831px legacy on an 800px window. The
    // old [300, 0.8×innerWidth] px clamp pinned the panel at 640px (80% of
    // the app); the band caps at (800−480)/800 = 0.4.
    expect(clampPanelFraction(831 / 800, 800)).toBe(0.4);
  });

  it("never exceeds half the window", () => {
    expect(clampPanelFraction(0.9, 2000)).toBe(0.5);
  });

  it("floors at the 300px panel minimum", () => {
    expect(clampPanelFraction(0.05, 1000)).toBe(0.3);
  });
});

describe("readRightPanelWidthFrac", () => {
  /** Stub `window` with a fake localStorage + viewport (node env has none). */
  function stubStorage(entries: Record<string, string>, innerWidth: number) {
    vi.stubGlobal("window", {
      innerWidth,
      localStorage: {
        getItem: (key: string) => entries[key] ?? null,
        setItem: () => {},
      },
    });
  }

  it("seeds from the legacy px key against the current viewport", () => {
    stubStorage({ "mh.rightPanelWidth": "831" }, 800);
    try {
      expect(readRightPanelWidthFrac()).toBe(0.4);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("prefers the fraction key over the legacy seed", () => {
    stubStorage(
      { "mh.rightPanelWidth": "831", "mh.rightPanelWidthFrac": "0.25" },
      800,
    );
    try {
      expect(readRightPanelWidthFrac()).toBe(0.25);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("returns null when nothing is persisted", () => {
    stubStorage({}, 800);
    try {
      expect(readRightPanelWidthFrac()).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

describe("readShowShellPreview", () => {
  /** Stub `window` with a fake localStorage (node env has none). */
  function stubStorage(entries: Record<string, string>) {
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (key: string) => entries[key] ?? null,
        setItem: () => {},
      },
    });
  }

  it("defaults to true when nothing is persisted", () => {
    stubStorage({});
    try {
      expect(readShowShellPreview()).toBe(true);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("reads an explicit false", () => {
    stubStorage({ "mh.showShellPreview": "false" });
    try {
      expect(readShowShellPreview()).toBe(false);
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
