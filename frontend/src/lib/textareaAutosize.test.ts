// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";

import { autosizeForScrollHeight } from "./textareaAutosize";

// Regression suite for the phantom gray scrollbar thumb at the right edge of
// the prompt input: at or below the cap a textarea must never show a
// vertical scrollbar (overflowY hidden — the border-box shortfall is
// absorbed by bottom padding); above the cap it must scroll (auto).
describe("autosizeForScrollHeight", () => {
  it("below the cap: unclamped height, overflow hidden (no phantom thumb)", () => {
    expect(autosizeForScrollHeight(38, 200)).toEqual({
      heightPx: 38,
      overflowY: "hidden",
    });
  });

  it("exactly at the cap: clamped to the cap, overflow hidden (equality fits)", () => {
    expect(autosizeForScrollHeight(200, 200)).toEqual({
      heightPx: 200,
      overflowY: "hidden",
    });
  });

  it("above the cap: height clamped, overflow auto (real scrolling)", () => {
    expect(autosizeForScrollHeight(340, 200)).toEqual({
      heightPx: 200,
      overflowY: "auto",
    });
  });
});
