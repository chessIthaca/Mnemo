// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Right-panel width contract (user report 2027-01-16, plan 38b0e2eb, BUG
 * memory 97200cbc): the persisted width must render as a FRACTION of the
 * window — never as absolute pixels. Two faces of the one flaw:
 *
 * 1. A px width persisted from a larger window (831px on the reporting
 *    box) pins at the render clamp's 80% ceiling whenever the restored
 *    window is smaller (the startup geometry clamp restores windows as
 *    small as 800×560): min(0.8 × 800, 831) = 640px — the panel fills
 *    80% of the app and the chat column is squeezed to a sliver.
 * 2. A px width ignores window resizes — the flex-1 chat column absorbed
 *    every resize delta while the panel stayed fixed.
 *
 * The fix renders the width as a percentage of the window with the band
 * caps inline — min(pct%, 50%, calc(100% - 480px)) — so it tracks
 * resizes AND the layout engine re-clamps the band at every viewport
 * (review L1). These tests pin that contract.
 *
 * Node-env harness (the repo's established pattern — Conversation.window
 * .test.tsx): renderToStaticMarkup makes useSyncExternalStore read the
 * store's INITIAL state, so setState is invisible — the legacy 831px is
 * seeded through a fake localStorage on a `window` stub installed via
 * vi.hoisted BEFORE the store module initializes (its create() reads the
 * persisted width at import time). The view registry is mocked empty so
 * the render stays trivial — the width contract lives on the root div.
 */

import { describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // Direct assignment — this must exist before any import evaluates.
  // innerWidth starts at the geometry clamp's minimum restore (800×560);
  // the fake localStorage carries the reporting box's real legacy value.
  (globalThis as { window?: unknown }).window = {
    innerWidth: 800,
    localStorage: {
      getItem: (key: string) => (key === "mh.rightPanelWidth" ? "831" : null),
      setItem: () => {},
    },
    matchMedia: () => ({ matches: false }),
  };
});

// No tabs — the width contract is on the root div; an empty registry also
// keeps the heavy view components (PlanProgress & co.) out of the render.
vi.mock("../../hooks/rightPanelViews", () => ({ RIGHT_PANEL_VIEWS: [] }));

import { renderToStaticMarkup } from "react-dom/server";
import { RightPanel } from "./RightPanel";

/** Point the stubbed window at a new viewport width. */
function setViewport(innerWidth: number): void {
  (globalThis as { window: { innerWidth: number } }).window.innerWidth =
    innerWidth;
}

/** Render the panel; return the root div's inline width style (e.g.
 * "640px" or "40%"), or undefined when it renders without one. */
function renderedWidth(): string | undefined {
  const markup = renderToStaticMarkup(<RightPanel />);
  const m = markup.match(/style="width:([^"]+)"/);
  return m?.[1];
}

describe("RightPanel width — a fraction of the window, not absolute px", () => {
  it("does not pin a legacy px width at the 80% cap on a small window", () => {
    // 831px persisted; the restored window is 800px wide (the geometry
    // clamp's minimum). Old behavior: min(0.8 × 800, 831) = 640px — the
    // panel fills 80% of the app.
    setViewport(800);
    const width = renderedWidth();
    // The width must be window-relative (percentages, not pinned px) and
    // carry the band caps inline — at most half the window, never past
    // the chat column's guaranteed minimum — so the layout engine
    // re-clamps at EVERY viewport, not just at render time (review L1).
    expect(width, `rendered width style was ${width}`).toMatch(/%/);
    expect(width).not.toMatch(/px$/);
    expect(width).toContain("50%");
    expect(width).toContain("calc(100% - 480px)");
  });

  it("expresses the width window-relative so resizes re-scale it, not fixed px", () => {
    // The same persisted width at two viewports must render as a
    // percentage — a percentage style re-scales with the window for free;
    // the old px style stayed fixed (800px at 1000, 831px at 1400) while
    // the chat column absorbed every resize delta.
    setViewport(1000);
    const at1000 = renderedWidth();
    setViewport(1400);
    const at1400 = renderedWidth();
    expect(at1000, `rendered width style at 1000 was ${at1000}`).toMatch(/%/);
    expect(at1400, `rendered width style at 1400 was ${at1400}`).toMatch(/%/);
  });
});
