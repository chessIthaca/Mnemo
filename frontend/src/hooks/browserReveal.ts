// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useAgentStore } from "./useAgentStore";

/**
 * Handler for the backend's `browser://reveal` event — emitted when the
 * agent's `browser_navigate` bootstraps the Browser tab's child webview
 * (plan 5ae26d22). Reveals + selects the Browser tab in the right panel so
 * the human sees the navigation the agent initiated instead of a hidden
 * webview loading invisibly.
 *
 * `revealRightPanelTab` never disables an already-enabled tab and no-ops the
 * selection when the tab is already active — a user browsing another tool
 * tab is only moved when the Browser tab was disabled/unselected.
 *
 * Extracted as a pure (React-free) function so the node-env unit tests can
 * exercise the store wiring directly (the Tauri `listen()` subscription
 * itself stays in useAgentEvents' idempotent module-scope listener, mirroring
 * the backlog listener).
 */
export function handleBrowserReveal(): void {
  useAgentStore.getState().revealRightPanelTab("browser");
}
