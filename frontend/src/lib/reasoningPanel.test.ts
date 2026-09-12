// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";
import { reasoningActive, reasoningBlockStarted } from "./reasoningPanel";
import type { ActivityEntry } from "../hooks/agentState";

/** A minimal reasoning activity entry. */
function reasoningEntry(text = "thinking…"): ActivityEntry {
  return { kind: "reasoning", text, timestamp: 1 };
}

describe("reasoningActive (panel auto-expand signal)", () => {
  it("is true while the live phase is reasoning, even with an empty log", () => {
    expect(reasoningActive("reasoning", [])).toBe(true);
  });

  it("stays true after the phase moves on while a reasoning entry remains", () => {
    // During the answer the phase is "streaming", but the log still holds the
    // reasoning block — the panel must stay open for the rest of the block.
    expect(reasoningActive("streaming", [reasoningEntry()])).toBe(true);
    expect(reasoningActive("running_tools", [reasoningEntry()])).toBe(true);
  });

  it("is false when the phase is not reasoning and the log has no reasoning entry", () => {
    expect(reasoningActive("sending", [])).toBe(false);
    expect(reasoningActive("waiting", [])).toBe(false);
    expect(
      reasoningActive("streaming", [{ kind: "answer", text: "hi", timestamp: 1 }]),
    ).toBe(false);
  });
});

describe("reasoningBlockStarted (one-shot edge)", () => {
  it("fires only on the inactive→active edge", () => {
    expect(reasoningBlockStarted(false, true)).toBe(true);
    expect(reasoningBlockStarted(true, true)).toBe(false); // stays active
    expect(reasoningBlockStarted(true, false)).toBe(false); // block ended
    expect(reasoningBlockStarted(false, false)).toBe(false); // still idle
  });

  it("re-arms after a block ends (manual collapse is respected until then)", () => {
    // The full lifecycle: a block starts (expand), the user collapses it
    // mid-block (flag stays true → no re-expand), the block ends, and the
    // NEXT block starts fresh (expand again).
    let was = false;
    expect(reasoningBlockStarted(was, true)).toBe(true);
    was = true;
    expect(reasoningBlockStarted(was, true)).toBe(false);
    was = false;
    expect(reasoningBlockStarted(was, true)).toBe(true);
  });
});
