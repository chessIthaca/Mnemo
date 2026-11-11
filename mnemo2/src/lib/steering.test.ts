import { describe, expect, it, vi } from "vitest";

import { isTauriRuntime } from "./runtime";
import {
  createSteeringTransport,
  createTauriSteeringTransport,
  followRate,
  hasSteeringData,
  MARKER_LABELS,
  markerLabel,
  steeringRows,
  type SteeringStats,
} from "./steering";

// A plain browser page has no host; the transport factory must agree.
vi.mock("./runtime", () => ({ isTauriRuntime: vi.fn(() => false) }));

/** A counters snapshot, all zeros unless a test says otherwise. */
function stats(overrides: Partial<SteeringStats> = {}): SteeringStats {
  return {
    fired: 0,
    switched: 0,
    by_marker: [],
    file_tool_mutations: 0,
    shell_mutations: 0,
    ...overrides,
  };
}

describe("steering transport", () => {
  it("is null outside the desktop app", () => {
    expect(createSteeringTransport()).toBeNull();
  });

  it("invokes trace_steering_stats", async () => {
    const invoke = vi.fn(async () => stats({ fired: 1 }));
    const transport = createTauriSteeringTransport({ invoke: invoke as never });
    await expect(transport.stats()).resolves.toEqual(stats({ fired: 1 }));
    expect(invoke).toHaveBeenCalledWith("trace_steering_stats");
  });

  it("builds a transport from injected bindings even without the host", () => {
    const transport = createSteeringTransport({ invoke: (async () => stats()) as never });
    expect(transport).not.toBeNull();
    expect(isTauriRuntime()).toBe(false);
  });
});

describe("followRate", () => {
  it("shows a dash when nothing fired", () => {
    expect(followRate(0, 0)).toBe("—");
  });

  it("rounds the followed share to a whole percent", () => {
    expect(followRate(4, 3)).toBe("75%");
    expect(followRate(3, 2)).toBe("67%");
    expect(followRate(2, 2)).toBe("100%");
  });
});

describe("steeringRows", () => {
  const row = (marker: string, fired: number) => ({ marker, fired, switched: 0, gated: 0 });

  it("drops kinds that never fired", () => {
    const rows = steeringRows(
      stats({
        by_marker: [row("search-nudge", 2), row("shell-tip", 0), row("read-nudge", 1)],
      }),
    );
    expect(rows.map((entry) => entry.marker)).toEqual(["search-nudge", "read-nudge"]);
  });

  it("sorts by how often a kind fired, then by label", () => {
    const rows = steeringRows(
      stats({
        by_marker: [
          row("read-nudge", 1),
          row("shell-tip", 3),
          row("graph-miss", 1),
          row("search-nudge", 3),
        ],
      }),
    );
    expect(rows.map((entry) => entry.marker)).toEqual([
      "search-nudge",
      "shell-tip",
      "graph-miss",
      "read-nudge",
    ]);
  });

  it("leaves the caller's snapshot untouched", () => {
    const snapshot = stats({ by_marker: [row("shell-tip", 1), row("search-nudge", 2)] });
    steeringRows(snapshot);
    expect(snapshot.by_marker.map((entry) => entry.marker)).toEqual(["shell-tip", "search-nudge"]);
  });
});

describe("markerLabel", () => {
  it("names the kinds the engine reports", () => {
    expect(markerLabel("search-nudge")).toBe("Use the code graph");
    expect(markerLabel("edit-stale-read")).toBe("Re-read before editing");
    expect(Object.keys(MARKER_LABELS)).toHaveLength(10);
  });

  it("falls back to the label itself", () => {
    expect(markerLabel("brand-new-nudge")).toBe("brand-new-nudge");
  });
});

describe("hasSteeringData", () => {
  it("is false with no snapshot and with everything at zero", () => {
    expect(hasSteeringData(null)).toBe(false);
    expect(hasSteeringData(stats())).toBe(false);
  });

  it("is true once any counter moved", () => {
    expect(hasSteeringData(stats({ fired: 1 }))).toBe(true);
    expect(hasSteeringData(stats({ shell_mutations: 1 }))).toBe(true);
  });
});
