// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";
import {
  applyIndexProgress,
  hideDelayMs,
  MIN_VISIBLE_MS,
  type IndexingOverlayState,
} from "./IndexingOverlay";
import type { IndexProgressEvent } from "../../lib/tauri";

/** Fold a fresh event stream through the reducer from the hidden state. */
function fold(events: IndexProgressEvent[]): IndexingOverlayState {
  let state: IndexingOverlayState = null;
  for (const event of events) state = applyIndexProgress(state, event);
  return state;
}

describe("applyIndexProgress (open-project indexing overlay)", () => {
  it("started makes the overlay visible in the indeterminate 0/0 state", () => {
    expect(applyIndexProgress(null, { type: "started" })).toEqual({
      kind: "progress",
      done: 0,
      total: 0,
    });
  });

  it("progress carries the counter values (1/1452)", () => {
    expect(
      applyIndexProgress(null, { type: "progress", done: 1, total: 1452 }),
    ).toEqual({ kind: "progress", done: 1, total: 1452 });
  });

  it("progress to completion reaches 1452/1452 and stays visible until done", () => {
    const state = fold([
      { type: "started" },
      { type: "progress", done: 700, total: 1452 },
      { type: "progress", done: 1452, total: 1452 },
    ]);
    expect(state).toEqual({ kind: "progress", done: 1452, total: 1452 });
  });

  it("done hides the overlay (the app is ready)", () => {
    const state = fold([
      { type: "started" },
      { type: "progress", done: 1452, total: 1452 },
      { type: "done", summary: "1452 files indexed · 9001 symbols" },
    ]);
    expect(state).toBeNull();
  });

  it("failed keeps the overlay visible with the error text", () => {
    const state = fold([
      { type: "started" },
      { type: "progress", done: 50, total: 1452 },
      { type: "failed", error: "db locked" },
    ]);
    expect(state).toEqual({ kind: "failed", error: "db locked" });
  });

  it("a started after a failure re-shows the progress state", () => {
    const state = fold([
      { type: "failed", error: "db locked" },
      { type: "started" },
      { type: "progress", done: 3, total: 10 },
    ]);
    expect(state).toEqual({ kind: "progress", done: 3, total: 10 });
  });

  it("done is idempotent from the hidden state (late event is a no-op)", () => {
    expect(applyIndexProgress(null, { type: "done", summary: "ok" })).toBeNull();
  });
});

describe("hideDelayMs (minimum-visible hold)", () => {
  const MIN = MIN_VISIBLE_MS;

  it("exports a positive minimum-visible window", () => {
    expect(MIN).toBeGreaterThan(0);
  });

  it("never shown → hide immediately", () => {
    expect(hideDelayMs(null, 12345)).toBe(0);
  });

  it("freshly shown → hold for the remaining window", () => {
    // Shown 300ms ago: 500ms of the 800ms minimum remain.
    expect(hideDelayMs(1000, 1300)).toBe(MIN - 300);
  });

  it("shown longer than the window → hide immediately", () => {
    expect(hideDelayMs(1000, 1000 + MIN)).toBe(0);
    expect(hideDelayMs(1000, 1000 + MIN + 5000)).toBe(0);
  });

  it("the boundary tick hides exactly at the window edge", () => {
    expect(hideDelayMs(1000, 1000 + MIN - 1)).toBe(1);
    expect(hideDelayMs(1000, 1000 + MIN)).toBe(0);
  });

  it("shown exactly now → hold the full window", () => {
    // The hold starts from the show instant, so a bar shown at this exact
    // tick keeps the full MIN_VISIBLE_MS window before hiding.
    expect(hideDelayMs(1000, 1000)).toBe(MIN);
  });
});