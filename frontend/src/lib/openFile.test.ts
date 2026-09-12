// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";

import { openDiffInViewer } from "./openFile";
import { useAgentStore } from "../hooks/useAgentStore";

/**
 * Regression (backlog 2026-08-22): a `file_edit` tool-card file-name click
 * must open the right-panel Diff tab at that file — not the Files tab like
 * every other tool.
 */
describe("openDiffInViewer", () => {
  it("reveals the Diff tab and selects the file's diff entry", () => {
    openDiffInViewer("src/x.rs");
    const s = useAgentStore.getState();
    expect(s.rightPanelTab).toBe("diff");
    expect(s.rightPanelVisible).toBe(true);
    expect(s.selectedDiffPath).toBe("src/x.rs");
  });
});
