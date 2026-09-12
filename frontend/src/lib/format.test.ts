// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Unit tests for the shared token formatters in `lib/format.ts`.
 *
 * Hybrid rule: under 10K comma-grouped raw ("1,234"), 10K–1M with a K
 * suffix ("12.3K"), 1M+ with an M suffix ("1.2M"), at most 1 decimal.
 */

import { describe, expect, it } from "vitest";

import { fmtPct, fmtTokens, fmtRate, fmtDuration } from "./format";

describe("fmtTokens", () => {
  it("leaves small values raw", () => {
    expect(fmtTokens(0)).toBe("0");
    expect(fmtTokens(999)).toBe("999");
  });
  it("comma-groups values under 10K", () => {
    expect(fmtTokens(1000)).toBe("1,000");
    expect(fmtTokens(1234.5)).toBe("1,234.5");
    expect(fmtTokens(9999)).toBe("9,999");
  });

  it("uses a K suffix from 10K to under 1M", () => {
    expect(fmtTokens(10000)).toBe("10K");
    expect(fmtTokens(12345)).toBe("12.3K");
    expect(fmtTokens(999949)).toBe("999.9K");
  });

  it("uses an M suffix at 1M and above", () => {
    expect(fmtTokens(1e6)).toBe("1M");
    expect(fmtTokens(1234567)).toBe("1.2M");
    expect(fmtTokens(1234567890)).toBe("1,234.6M");
  });

  it("rolls over when rounding crosses a suffix boundary", () => {
    expect(fmtTokens(9999.96)).toBe("10K");
    expect(fmtTokens(999950)).toBe("1M");
    expect(fmtTokens(999999.5)).toBe("1M");
  });

  it("preserves the sign", () => {
    expect(fmtTokens(-1234)).toBe("-1,234");
  });

  it("never renders -0", () => {
    expect(fmtTokens(-0.04)).toBe("0");
  });
});

describe("fmtDuration", () => {
  it("shows bare seconds under a minute", () => {
    expect(fmtDuration(0)).toBe("0s");
    expect(fmtDuration(999)).toBe("0s");
    expect(fmtDuration(22_000)).toBe("22s");
    expect(fmtDuration(59_999)).toBe("59s");
  });

  it("shows minutes + seconds under an hour", () => {
    expect(fmtDuration(60_000)).toBe("1m 0s");
    expect(fmtDuration(63_000)).toBe("1m 3s");
    expect(fmtDuration(3_599_999)).toBe("59m 59s");
  });

  it("shows hours + minutes + seconds beyond an hour", () => {
    expect(fmtDuration(3_600_000)).toBe("1h 0m 0s");
    expect(fmtDuration(3_723_000)).toBe("1h 2m 3s");
  });

  it("clamps negative inputs to zero", () => {
    expect(fmtDuration(-5000)).toBe("0s");
  });
});

describe("fmtRate", () => {
  it("shows at most 1 decimal for small rates", () => {
    expect(fmtRate(42.55)).toBe("42.6");
    expect(fmtRate(0)).toBe("0");
  });

  it("applies the same hybrid suffixes to rates", () => {
    expect(fmtRate(1234.5)).toBe("1,234.5");
    expect(fmtRate(12000)).toBe("12K");
  });
});

describe("fmtPct", () => {
  it("keeps one decimal digit for fractional percentages (backlog 9042b47c)", () => {
    expect(fmtPct(99.3)).toBe("99.3");
    expect(fmtPct(54.98)).toBe("55");
    expect(fmtPct(54.94)).toBe("54.9");
    expect(fmtPct(6.66)).toBe("6.7");
  });

  it("rounds to 100 and keeps integers integral", () => {
    expect(fmtPct(99.96)).toBe("100");
    expect(fmtPct(100)).toBe("100");
    expect(fmtPct(55)).toBe("55");
  });

  it("never renders a trailing .0 or -0", () => {
    expect(fmtPct(0)).toBe("0");
    expect(fmtPct(0.04)).toBe("0");
    expect(fmtPct(-0)).toBe("0");
  });
});
