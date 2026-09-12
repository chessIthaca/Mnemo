// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * PlanProgress step-expansion wiring — source contracts (backlog
 * af572504). The vitest suite runs in a node environment, so the
 * interactive wiring (auto-expand of the active step, manual override
 * toggles, PlanStepRow usage in both views) is pinned against the
 * component source (InflightBar ?raw pattern). PlanStepRow's markup is
 * pinned in PlanStepRow.test.tsx.
 */
import { describe, expect, it } from "vitest";
import source from "./PlanProgress.tsx?raw";

describe("PlanProgress step expansion (source contract)", () => {
  it("auto-expands the first not-done step while executing", () => {
    expect(source).toContain('state === "executing"');
    expect(source).toContain("plan.steps.find((s) => !s.done)");
  });

  it("defaults expansion to the active step, with manual overrides", () => {
    expect(source).toContain(
      "expandOverrides[step.index] ?? step.index === activeIndex",
    );
    expect(source).toContain("!isExpanded(step)");
  });

  it("renders PlanStepRow in both the active-plan and ancestor views", () => {
    expect((source.match(/<PlanStepRow/g) ?? []).length).toBe(2);
  });

  it("resets manual overrides when a different plan loads", () => {
    // Signature-keyed reset (title + step count): stale index-keyed
    // overrides must not leak across plans / update_plan restructures.
    expect(source).toContain("lastPlanSignature");
    expect(source).toContain("setExpandOverrides({})");
  });

  it("defaults ancestor-view rows to collapsed", () => {
    expect(source).toContain("ancestorOverrides[step.index] ?? false");
  });
});
