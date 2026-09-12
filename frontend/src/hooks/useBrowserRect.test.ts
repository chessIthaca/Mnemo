// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect } from "vitest";
import { rafCoalesce } from "./useBrowserRect";

/**
 * Regression guard for R3 (2026-08-18 RDP freeze diagnosis): ResizeObserver /
 * window-resize bursts must coalesce to ONE rect report per animation frame,
 * so an RDP resize storm cannot flood the backend with WebView2 controller
 * calls. raf/caf are stubbed with captured frames — no DOM needed.
 */

/** Minimal raf/caf pair that captures scheduled frames for manual firing. */
function makeRaf() {
  const scheduled: Array<{ id: number; cb: () => void }> = [];
  let nextId = 1;
  const raf = (cb: () => void): number => {
    const id = nextId++;
    scheduled.push({ id, cb });
    return id;
  };
  const caf = (id: number): void => {
    const i = scheduled.findIndex((s) => s.id === id);
    if (i >= 0) scheduled.splice(i, 1);
  };
  /** Run every currently scheduled frame exactly once. */
  const flush = (): void => {
    for (const s of scheduled.splice(0)) s.cb();
  };
  return { raf, caf, flush };
}

describe("rafCoalesce (R3 rect-report throttle regression)", () => {
  it("runs fn exactly once per frame no matter how many call()s queue", () => {
    const { raf, caf, flush } = makeRaf();
    let calls = 0;
    const c = rafCoalesce(
      () => {
        calls++;
      },
      raf,
      caf,
    );
    c.call();
    c.call();
    c.call();
    flush();
    expect(calls).toBe(1);
  });

  it("cancel() before the frame fires drops the pending call", () => {
    const { raf, caf, flush } = makeRaf();
    let calls = 0;
    const c = rafCoalesce(
      () => {
        calls++;
      },
      raf,
      caf,
    );
    c.call();
    c.cancel();
    flush();
    expect(calls).toBe(0);
  });

  it("schedules again after a frame has run (the next burst gets a new frame)", () => {
    const { raf, caf, flush } = makeRaf();
    let calls = 0;
    const c = rafCoalesce(
      () => {
        calls++;
      },
      raf,
      caf,
    );
    c.call();
    flush();
    expect(calls).toBe(1);
    c.call();
    c.call();
    flush();
    expect(calls).toBe(2);
  });
});
