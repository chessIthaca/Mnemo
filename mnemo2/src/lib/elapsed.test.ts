import { describe, expect, it } from "vitest";
import { formatAgo, turnStartTimes } from "./elapsed";

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

describe("turnStartTimes", () => {
  it("measures from the first tool of each turn", () => {
    expect(
      turnStartTimes([
        { id: "user-1", kind: "message", author: "user" },
        { id: "tool-a", kind: "tool", startedAtMs: 1000 },
        { id: "tool-b", kind: "tool", startedAtMs: 5000 },
        { id: "user-2", kind: "message", author: "user" },
        { id: "tool-c", kind: "tool", startedAtMs: 9000 },
      ]),
    ).toEqual({ "tool-a": 1000, "tool-b": 1000, "tool-c": 9000 });
  });

  it("starts a new worker turn at a lead message", () => {
    expect(
      turnStartTimes([
        { id: "tool-a", kind: "tool", startedAtMs: 100 },
        { id: "lead-1", kind: "message", from: "lead" },
        { id: "tool-b", kind: "tool", startedAtMs: 700 },
      ]),
    ).toEqual({ "tool-a": 100, "tool-b": 700 });
  });

  it("keeps the turn running across worker messages", () => {
    expect(
      turnStartTimes([
        { id: "tool-a", kind: "tool", startedAtMs: 100 },
        { id: "worker-1", kind: "message", from: "worker" },
        { id: "note", from: "harness" },
        { id: "tool-b", kind: "tool", startedAtMs: 700 },
      ]),
    ).toEqual({ "tool-a": 100, "tool-b": 100 });
  });

  it("skips tools without a start time", () => {
    expect(
      turnStartTimes([
        { id: "tool-a", kind: "tool" },
        { id: "tool-b", kind: "tool", startedAtMs: 400 },
      ]),
    ).toEqual({ "tool-b": 400 });
  });
});