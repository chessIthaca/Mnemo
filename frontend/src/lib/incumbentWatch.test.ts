// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { watchIncumbentLiveness } from "./incumbentWatch";
import appSource from "../App.tsx?raw";

describe("watchIncumbentLiveness", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("stays silent while the incumbent is alive", async () => {
    const onDead = vi.fn();
    const isAlive = vi.fn().mockResolvedValue(true);
    watchIncumbentLiveness(4242, isAlive, onDead, 2000);
    await vi.advanceTimersByTimeAsync(6000);
    expect(isAlive).toHaveBeenCalledTimes(3);
    expect(onDead).not.toHaveBeenCalled();
  });

  it("fires onDead once and stops watching once the pid is dead", async () => {
    const onDead = vi.fn();
    const isAlive = vi.fn().mockResolvedValue(false);
    watchIncumbentLiveness(4242, isAlive, onDead, 2000);
    await vi.advanceTimersByTimeAsync(2000);
    expect(onDead).toHaveBeenCalledTimes(1);
    // The watch stopped with the retraction — no further probes fire.
    const probes = isAlive.mock.calls.length;
    await vi.advanceTimersByTimeAsync(6000);
    expect(isAlive.mock.calls.length).toBe(probes);
    expect(onDead).toHaveBeenCalledTimes(1);
  });

  it("treats a rejected probe as unknown liveness and keeps watching", async () => {
    const onDead = vi.fn();
    const isAlive = vi
      .fn()
      .mockRejectedValueOnce(new Error("ipc down"))
      .mockResolvedValue(false);
    watchIncumbentLiveness(4242, isAlive, onDead, 2000);
    await vi.advanceTimersByTimeAsync(2000);
    // The rejection did NOT retract the dialog...
    expect(onDead).not.toHaveBeenCalled();
    // ...but a definitive dead probe on a later tick still does.
    await vi.advanceTimersByTimeAsync(2000);
    expect(onDead).toHaveBeenCalledTimes(1);
  });

  it("stop() cancels the watch before any probe runs", async () => {
    const onDead = vi.fn();
    const isAlive = vi.fn().mockResolvedValue(false);
    const stop = watchIncumbentLiveness(4242, isAlive, onDead, 2000);
    stop();
    await vi.advanceTimersByTimeAsync(10000);
    expect(isAlive).not.toHaveBeenCalled();
    expect(onDead).not.toHaveBeenCalled();
  });

  it("re-checks immediately on window focus and unregisters afterwards", async () => {
    const listeners: Record<string, (e: unknown) => void> = {};
    vi.stubGlobal("window", {
      addEventListener: (type: string, fn: (e: unknown) => void) => {
        listeners[type] = fn;
      },
      removeEventListener: (type: string) => {
        delete listeners[type];
      },
    });
    const onDead = vi.fn();
    const isAlive = vi.fn().mockResolvedValue(false);
    watchIncumbentLiveness(4242, isAlive, onDead, 2000);
    // No interval tick yet — focus alone triggers the re-check.
    listeners["focus"]({});
    await vi.advanceTimersByTimeAsync(0);
    expect(isAlive).toHaveBeenCalledTimes(1);
    expect(onDead).toHaveBeenCalledTimes(1);
    // Retraction unregisters the focus listener.
    expect(listeners["focus"]).toBeUndefined();
  });

  it("App.tsx wires the watch to the conflict retraction (source contract)", () => {
    // Red before the wiring landed: neither the watch import nor the probe
    // existed in App.tsx (backlog ea9576d7 acceptance — closing the
    // warned-about instance retracts the surviving instance's warning
    // without a restart).
    expect(appSource).toContain("watchIncumbentLiveness");
    expect(appSource).toContain("isIncumbentAlive");
  });
});