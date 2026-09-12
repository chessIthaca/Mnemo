// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * MemoryDebugView — the Memory tab's list-staleness contract, pinned via
 * static source assertions (the ModelCombobox.test.tsx pattern — vitest runs
 * in node, no DOM): the overview poll piggybacks a list re-fetch when a tier
 * count changes, reusing the same loadList fetch as the tier filter.
 */

import { describe, expect, it } from "vitest";
import source from "./MemoryDebugView.tsx?raw";

describe("MemoryDebugView list staleness (source contracts)", () => {
  it("the overview tick piggybacks a list re-fetch on a counts change", () => {
    expect(source).toContain("lastCountsSig");
    expect(source).toContain("JSON.stringify(ov.counts)");
    expect(source).toContain("void loadList()");
  });

  it("the tier-filter effect reuses the same loadList fetch", () => {
    expect(source).toContain("void loadList(() => cancelled)");
  });
});
