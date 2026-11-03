import { describe, expect, it } from "vitest";
import type { WorkerRouting } from "./types";
import { nudgeChipText, routingChipText } from "./workerRouting";

/**
 * The mission-control routing chips (plan 2ba2168a, design §4.10): the wording
 * the worker card shows for a dispatch decision and for a live nudge.
 */

/** One decision, with per-test overrides. */
function routing(overrides: Partial<WorkerRouting> = {}): WorkerRouting {
  return {
    laya_tier: "average",
    confidence: 0.62,
    cleared: true,
    lead_tier: null,
    routed_by: "laya",
    ran_tier: "average",
    nudged: null,
    ...overrides,
  };
}

describe("routingChipText", () => {
  it("reads the cleared tier with its confidence", () => {
    expect(routingChipText(routing())).toBe("Laya: average 0.62");
  });

  it("drops the confidence when the answer carried none", () => {
    expect(routingChipText(routing({ confidence: null }))).toBe("Laya: average");
  });

  it("names the lead's tier when Laya did not clear the gate", () => {
    expect(routingChipText(routing({ cleared: false, laya_tier: "difficult", lead_tier: "difficult" }))).toBe(
      "lead chose difficult",
    );
  });

  it("falls back to the tier that ran, then to 'Laya unsure'", () => {
    expect(routingChipText(routing({ cleared: false, laya_tier: null, lead_tier: null, ran_tier: "simple" }))).toBe(
      "lead chose simple",
    );
    expect(
      routingChipText(routing({ cleared: false, laya_tier: null, lead_tier: null, ran_tier: null })),
    ).toBe("Laya unsure");
  });

  it("renders nothing without a decision", () => {
    expect(routingChipText(null)).toBeNull();
    expect(routingChipText(undefined)).toBeNull();
  });
});

describe("nudgeChipText", () => {
  it("shows the suggested tier while the worker runs elsewhere", () => {
    expect(nudgeChipText(routing({ nudged: "difficult" }), "average", "working")).toBe(
      "Laya suggests difficult — stuck",
    );
  });

  it("clears once the worker runs on the suggested tier", () => {
    expect(nudgeChipText(routing({ nudged: "difficult" }), "difficult", "working")).toBeNull();
  });

  it("clears when the run finished", () => {
    for (const status of ["merged", "failed", "stopped", "completed"] as const) {
      expect(nudgeChipText(routing({ nudged: "difficult" }), "average", status)).toBeNull();
    }
  });

  it("renders nothing without a nudge", () => {
    expect(nudgeChipText(routing(), "average", "working")).toBeNull();
    expect(nudgeChipText(null, "average", "working")).toBeNull();
  });
});
