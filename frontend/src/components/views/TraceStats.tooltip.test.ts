// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Source-contract test (InflightBar.test.ts precedent): the chart columns
 * must render the custom multiline tooltip (portal to <body>, structured
 * tip rows) instead of a native single-line `title` — the graph tooltips
 * were unreadable `·`-joined one-liners (user request 2026-08-22: multiline
 * + aligned). The row CONTENT is unit-tested in lib/traceStats.test.ts; this
 * pins the wiring.
 */
import { describe, expect, it } from "vitest";
import source from "./TraceStats.tsx?raw";

describe("TraceStats column tooltips", () => {
  it("render a portal-based custom tooltip on hover", () => {
    expect(source).toContain("createPortal(");
    expect(source).toContain("onMouseEnter");
    expect(source).toContain("tipRows");
  });

  it("build tooltip content via the pure row builders", () => {
    expect(source).toContain("phaseTipRows(");
    expect(source).toContain("tokenTipRows(");
  });

  it("do not use a native single-line title on the chart columns", () => {
    // The old `·`-joined one-liner must be gone (native titles can't align).
    expect(source).not.toContain("· prep ${");
    expect(source).not.toContain("(cached ${fmtInt(");
  });
});
