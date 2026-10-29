import { describe, expect, it } from "vitest";
import {
  formatTokenCount,
  formatUsageCost,
  usageIsPartial,
  usageTotalTokens,
} from "./usage";
import type { ProjectUsage } from "./types";

const usage = (extra: Partial<ProjectUsage> = {}): ProjectUsage => ({
  inputTokens: 900,
  cachedTokens: 300,
  outputTokens: 120,
  costMicros: 1_234_567,
  unpricedModels: [],
  ...extra,
});

describe("usage formatting", () => {
  it("always renders a USD cost, including zero", () => {
    expect(formatUsageCost(0)).toBe("$0.00");
    expect(formatUsageCost(1_234_567)).toBe("$1.23");
    expect(formatUsageCost(4_320_000)).toBe("$4.32");
  });

  it("keeps sub-cent spend visible instead of rounding it to free", () => {
    expect(formatUsageCost(4_000)).toBe("$0.0040");
    expect(formatUsageCost(100)).toBe("$0.0001");
    expect(formatUsageCost(1)).toBe("<$0.0001");
  });

  it("never renders a negative or malformed cost", () => {
    expect(formatUsageCost(-500)).toBe("$0.00");
    expect(formatUsageCost(Number.NaN)).toBe("$0.00");
  });

  it("groups token counts", () => {
    expect(formatTokenCount(0)).toBe("0");
    expect(formatTokenCount(48_620)).toBe("48,620");
    expect(formatTokenCount(-3)).toBe("0");
  });

  it("sums cached, uncached input, and output tokens", () => {
    expect(usageTotalTokens(usage())).toBe(1_320);
    expect(usageTotalTokens(usage({ inputTokens: 0, cachedTokens: 0, outputTokens: 0 }))).toBe(0);
  });

  it("marks usage partial only when a model has no price", () => {
    expect(usageIsPartial(usage())).toBe(false);
    expect(usageIsPartial(usage({ unpricedModels: ["mystery-model"] }))).toBe(true);
  });
});
