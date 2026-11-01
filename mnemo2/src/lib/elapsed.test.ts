import { describe, expect, it } from "vitest";
import { formatAgo } from "./elapsed";

const NOW = 1_787_400_000_000;

describe("formatAgo", () => {
  it("stamps an age from seconds to days", () => {
    expect(formatAgo(NOW, NOW)).toBe("just now");
    expect(formatAgo(NOW - 59_000, NOW)).toBe("just now");
    expect(formatAgo(NOW - 60_000, NOW)).toBe("1m ago");
    expect(formatAgo(NOW - 26 * 60_000, NOW)).toBe("26m ago");
    expect(formatAgo(NOW - 3_600_000, NOW)).toBe("1h ago");
    expect(formatAgo(NOW - 3 * 3_600_000, NOW)).toBe("3h ago");
    expect(formatAgo(NOW - 26 * 3_600_000, NOW)).toBe("1d ago");
    expect(formatAgo(NOW - 3 * 86_400_000, NOW)).toBe("3d ago");
  });

  it("never reports a future clock as older than now", () => {
    expect(formatAgo(NOW + 5_000, NOW)).toBe("just now");
  });
});