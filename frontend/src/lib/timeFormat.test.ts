// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * timeFormat — unit tests for the shared chat time formatters.
 *
 * fmtToolDuration boundaries (the tool-card timing suffix) and fmtTs's
 * today-vs-older split (the hover tooltip + the card's wall-clock anchor).
 */

import { describe, expect, it } from "vitest";
import { fmtToolDuration, fmtTs } from "./timeFormat";

describe("fmtToolDuration", () => {
  it("renders sub-second durations in ms", () => {
    expect(fmtToolDuration(0)).toBe("0ms");
    expect(fmtToolDuration(999)).toBe("999ms");
  });

  it("renders sub-minute durations in one-decimal seconds", () => {
    expect(fmtToolDuration(1000)).toBe("1.0s");
    expect(fmtToolDuration(2300)).toBe("2.3s");
    expect(fmtToolDuration(59_900)).toBe("59.9s");
  });

  it("renders minute-plus durations as minutes and seconds", () => {
    expect(fmtToolDuration(60_000)).toBe("1m 0s");
    expect(fmtToolDuration(61_000)).toBe("1m 1s");
    expect(fmtToolDuration(72_500)).toBe("1m 13s");
  });

  it("carries a rounded 60s into the minutes", () => {
    // 1m 59.999s must not render as "1m 60s".
    expect(fmtToolDuration(119_999)).toBe("2m 0s");
  });
});

describe("fmtTs", () => {
  it("renders time-only for today's timestamps", () => {
    const now = Date.now();
    expect(fmtTs(now)).toBe(new Date(now).toLocaleTimeString());
  });

  it("renders date+time for older timestamps", () => {
    // 24h back is always a previous calendar day.
    const older = Date.now() - 24 * 60 * 60 * 1000;
    expect(fmtTs(older)).toBe(new Date(older).toLocaleString());
  });
});
