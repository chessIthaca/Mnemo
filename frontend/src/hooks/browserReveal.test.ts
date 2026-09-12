// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Unit tests for the `browser://reveal` handler (frontend/src/hooks/
 * browserReveal.ts) — the frontend half of the agent's browser_navigate
 * bootstrap (plan 5ae26d22). The backend emits `browser://reveal` when the
 * agent creates the Browser tab's child webview; the handler must reveal +
 * select the Browser tab (enable-if-disabled, select, show the panel) and
 * NEVER disable an already-enabled tab.
 */

import { beforeEach, describe, expect, it } from "vitest";

import { useAgentStore } from "./useAgentStore";
import { handleBrowserReveal } from "./browserReveal";
import type { RightPanelTab } from "./agentState";

function setTabs(partial: {
  rightPanelTab?: RightPanelTab;
  disabledTabs?: RightPanelTab[];
  rightPanelVisible?: boolean;
}): void {
  useAgentStore.setState({
    rightPanelTab: "plan",
    disabledTabs: [],
    rightPanelVisible: true,
    ...partial,
  });
}

describe("handleBrowserReveal", () => {
  beforeEach(() => {
    setTabs({});
  });

  it("enables a disabled Browser tab, selects it, and reveals the panel", () => {
    setTabs({ rightPanelTab: "plan", disabledTabs: ["browser"], rightPanelVisible: false });
    handleBrowserReveal();
    const s = useAgentStore.getState();
    expect(s.rightPanelTab).toBe("browser");
    expect(s.disabledTabs).not.toContain("browser");
    expect(s.rightPanelVisible).toBe(true);
  });

  it("selects the tab and reveals the panel when enabled but unselected", () => {
    setTabs({ rightPanelTab: "files", disabledTabs: [], rightPanelVisible: false });
    handleBrowserReveal();
    const s = useAgentStore.getState();
    expect(s.rightPanelTab).toBe("browser");
    expect(s.rightPanelVisible).toBe(true);
  });

  it("NEVER disables an already-enabled Browser tab (regression vs toggleTabAndReveal)", () => {
    setTabs({ rightPanelTab: "browser", disabledTabs: [], rightPanelVisible: true });
    handleBrowserReveal();
    const s = useAgentStore.getState();
    expect(s.rightPanelTab).toBe("browser");
    expect(s.disabledTabs).not.toContain("browser");
    expect(s.rightPanelVisible).toBe(true);
  });
});
