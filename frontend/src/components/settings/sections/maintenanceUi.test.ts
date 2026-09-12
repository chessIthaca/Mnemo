// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it, vi } from "vitest";
import type { MaintenanceEvent } from "../../../lib/tauri";
import {
  applyMaintenanceEvent,
  idleOp,
  subscribeUntilDisposed,
} from "./maintenanceUi";

/** Let pending promise callbacks (the `.then` on the listen promise) run. */
async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

/** Pins the `memory://maintenance` event → UI-state contract the MemorySection
 *  progress bar renders from (wire shape pinned on the backend by
 *  `maintenance_event_wire_shape`). */
describe("applyMaintenanceEvent", () => {
  it("started flips to a fresh running state, clearing any previous result", () => {
    const stale = { ...idleOp(), summary: "previous run" };
    const s = applyMaintenanceEvent(stale, { type: "started", op: "cleanup" });
    expect(s).toEqual({ ...idleOp(), running: true });
  });

  it("progress updates the bar numbers and stays running", () => {
    const s = applyMaintenanceEvent(idleOp(), {
      type: "progress",
      op: "rebuild",
      done: 2,
      total: 5,
      phase: "embedding",
    });
    expect(s.running).toBe(true);
    expect(s.done).toBe(2);
    expect(s.total).toBe(5);
    expect(s.phase).toBe("embedding");
  });

  it("label-only tail phases carry total = 0 (indeterminate bar)", () => {
    const s = applyMaintenanceEvent(idleOp(), {
      type: "progress",
      op: "rebuild",
      done: 0,
      total: 0,
      phase: "reindexing",
    });
    expect(s.running).toBe(true);
    expect(s.total).toBe(0);
    expect(s.phase).toBe("reindexing");
  });

  it("done stops the bar and surfaces the summary", () => {
    let s = applyMaintenanceEvent(idleOp(), { type: "started", op: "cleanup" });
    s = applyMaintenanceEvent(s, {
      type: "progress",
      op: "cleanup",
      done: 1,
      total: 1,
      phase: "consolidating",
    });
    s = applyMaintenanceEvent(s, {
      type: "done",
      op: "cleanup",
      summary: "3 session(s) consolidated",
    });
    expect(s.running).toBe(false);
    expect(s.phase).toBe("");
    expect(s.summary).toBe("3 session(s) consolidated");
    expect(s.error).toBeNull();
  });

  it("failed stops the bar and surfaces the error, clearing any summary", () => {
    let s = applyMaintenanceEvent(idleOp(), { type: "started", op: "rebuild" });
    s = applyMaintenanceEvent(s, {
      type: "failed",
      op: "rebuild",
      error: "boom",
    });
    expect(s.running).toBe(false);
    expect(s.phase).toBe("");
    expect(s.error).toBe("boom");
    expect(s.summary).toBeNull();
  });

  it("a restart after a terminal state resets cleanly", () => {
    let s = applyMaintenanceEvent(idleOp(), {
      type: "done",
      op: "cleanup",
      summary: "old summary",
    });
    s = applyMaintenanceEvent(s, { type: "started", op: "cleanup" });
    expect(s).toEqual({ ...idleOp(), running: true });
  });
});

describe("subscribeUntilDisposed", () => {
  it("invokes the resolved unlisten immediately when disposed before the listen promise resolves (B1)", async () => {
    // Regression pin for the listener leak: dispose (unmount) fires while the
    // subscribe promise is still pending; when it later resolves, the
    // unlisten must be called on the spot or the listener leaks forever.
    let resolveListen: (unlisten: () => void) => void = () => {};
    const listen = new Promise<() => void>((resolve) => {
      resolveListen = resolve;
    });
    const unlisten = vi.fn();
    const dispose = subscribeUntilDisposed(() => listen, () => {});
    dispose(); // unmount before the listen promise resolves
    resolveListen(unlisten);
    await flushMicrotasks();
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("holds the subscription while mounted, delivers events, and unsubscribes on later dispose", async () => {
    const unlisten = vi.fn();
    const received: MaintenanceEvent[] = [];
    const handlers: ((e: MaintenanceEvent) => void)[] = [];
    const dispose = subscribeUntilDisposed<MaintenanceEvent>(
      (handler) => {
        handlers[0] = handler;
        return Promise.resolve(unlisten);
      },
      (e) => received.push(e),
    );
    // While mounted: an event pushed through the listener is delivered.
    handlers[0]?.({ type: "started", op: "cleanup" });
    expect(received).toEqual([{ type: "started", op: "cleanup" }]);
    expect(unlisten).not.toHaveBeenCalled();
    // Let the listen promise's .then run (still not disposed → the unlisten
    // is held), then dispose → it is invoked synchronously.
    await flushMicrotasks();
    dispose();
    expect(unlisten).toHaveBeenCalledOnce();
  });
});

/** Pins the `codegraph://maintenance` rebuild stream → UI-state contract:
 *  these events carry no `op` router field but fold through the same mapping
 *  (wire shape pinned on the backend by `reindex_event_wire_shape`). */
describe("applyMaintenanceEvent — codegraph rebuild stream", () => {
  it("started flips to a fresh running state, clearing any previous result", () => {
    const stale = { ...idleOp(), summary: "previous run" };
    const s = applyMaintenanceEvent(stale, { type: "started" });
    expect(s).toEqual({ ...idleOp(), running: true });
  });

  it("progress updates the bar numbers and stays running", () => {
    const s = applyMaintenanceEvent(idleOp(), {
      type: "progress",
      done: 40,
      total: 120,
      phase: "indexing",
    });
    expect(s.running).toBe(true);
    expect(s.done).toBe(40);
    expect(s.total).toBe(120);
    expect(s.phase).toBe("indexing");
  });

  it("done stops the bar and surfaces the summary", () => {
    let s = applyMaintenanceEvent(idleOp(), { type: "started" });
    s = applyMaintenanceEvent(s, {
      type: "progress",
      done: 120,
      total: 120,
      phase: "indexing",
    });
    s = applyMaintenanceEvent(s, {
      type: "done",
      summary: "120 files scanned · 3 re-parsed",
    });
    expect(s.running).toBe(false);
    expect(s.phase).toBe("");
    expect(s.summary).toBe("120 files scanned · 3 re-parsed");
    expect(s.error).toBeNull();
  });

  it("failed stops the bar and surfaces the error, clearing any summary", () => {
    let s = applyMaintenanceEvent(idleOp(), { type: "started" });
    s = applyMaintenanceEvent(s, { type: "failed", error: "boom" });
    expect(s.running).toBe(false);
    expect(s.error).toBe("boom");
    expect(s.summary).toBeNull();
  });
});
