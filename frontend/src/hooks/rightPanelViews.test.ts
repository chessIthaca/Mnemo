// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Unit tests for the right-panel view registry (Maint M6).
 *
 * The registry is the single source of truth for tab display + render
 * metadata. These tests guard against drift between the `RightPanelTab` union
 * (the store's persisted type) and the registry: every tab id must have a
 * registry entry (no missing view), and the registry must not reference an
 * unknown id.
 */

import { describe, expect, it } from "vitest";

import { ALL_RIGHT_PANEL_TABS } from "./agentState";
import type { RightPanelTab } from "./agentState";
import { RIGHT_PANEL_VIEWS } from "./rightPanelViews";

describe("RIGHT_PANEL_VIEWS registry", () => {
  it("has an entry for every RightPanelTab id", () => {
    // Every persisted tab id must have display + render metadata in the
    // registry, otherwise a persisted/active tab would render nothing.
    const registryIds = new Set(RIGHT_PANEL_VIEWS.map((v) => v.id));
    for (const tab of ALL_RIGHT_PANEL_TABS as RightPanelTab[]) {
      expect(registryIds.has(tab), `missing registry entry for tab '${tab}'`).toBe(true);
    }
  });

  it("does not reference any id outside the RightPanelTab union", () => {
    // The registry must not introduce an id the store doesn't know about
    // (would break disabledTabs persistence + the active-tab fallback).
    const known = new Set<RightPanelTab>(ALL_RIGHT_PANEL_TABS as RightPanelTab[]);
    for (const view of RIGHT_PANEL_VIEWS) {
      expect(known.has(view.id), `registry has unknown tab id '${view.id}'`).toBe(true);
    }
  });

  it("has the same number of entries as ALL_RIGHT_PANEL_TABS (no dupes, no gaps)", () => {
    expect(RIGHT_PANEL_VIEWS.length).toBe(ALL_RIGHT_PANEL_TABS.length);
  });

  it("every entry has a non-empty label, an icon, and a component", () => {
    for (const view of RIGHT_PANEL_VIEWS) {
      expect(view.label.length).toBeGreaterThan(0);
      // Icons (lucide) + view components are React components — objects or
      // functions depending on the wrapper. Just assert they're present.
      expect(view.icon).toBeTruthy();
      expect(view.component).toBeTruthy();
    }
  });
});
