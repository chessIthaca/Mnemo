// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * windowGeometryFlush — regression tests for the pre-restart geometry flush.
 *
 * Defect (user report 2027-01-24): switching projects restarts the app with
 * a hard process exit — no `beforeunload` runs — so a window move/resize
 * made within the 400 ms save debounce was lost, and the relaunched window
 * opened at the previous bounds. The picker must flush the geometry save
 * right before `switchProject`; the flusher itself is registered by App's
 * geometry effect (pinned in windowRestore.test.ts).
 */

import { describe, expect, it, vi } from "vitest";
import {
  flushWindowGeometry,
  registerWindowGeometryFlusher,
} from "./windowGeometryFlush";
import pickerSource from "../components/projects/ProjectPicker.tsx?raw";

describe("windowGeometryFlush registry", () => {
  it("invokes the registered flusher", async () => {
    const flusher = vi.fn().mockResolvedValue(undefined);
    registerWindowGeometryFlusher(flusher);
    await flushWindowGeometry();
    expect(flusher).toHaveBeenCalledOnce();
  });

  it("is a no-op with no flusher registered", async () => {
    registerWindowGeometryFlusher(null);
    await expect(flushWindowGeometry()).resolves.toBeUndefined();
  });

  it("clearing with null stops later flushes", async () => {
    const flusher = vi.fn().mockResolvedValue(undefined);
    registerWindowGeometryFlusher(flusher);
    registerWindowGeometryFlusher(null);
    await flushWindowGeometry();
    expect(flusher).not.toHaveBeenCalled();
  });
});

describe("ProjectPicker flush wiring (source contract)", () => {
  it("flushes the geometry save before every switchProject restart", () => {
    // Both switch paths (open existing, create new) must flush the window
    // geometry right before switchProject — the hard restart never runs
    // beforeunload, so the flush is the only way the latest bounds survive
    // the 400 ms debounce.
    expect(pickerSource.split("await flushWindowGeometry()")).toHaveLength(3);
    expect(pickerSource.split("await switchProject(")).toHaveLength(3);
    // Each switch is preceded by its flush, in order.
    const openFlush = pickerSource.indexOf("await flushWindowGeometry()");
    const openSwitch = pickerSource.indexOf("await switchProject(");
    expect(openFlush).toBeGreaterThan(-1);
    expect(openSwitch).toBeGreaterThan(openFlush);
    const createFlush = pickerSource.indexOf(
      "await flushWindowGeometry()",
      openSwitch,
    );
    const createSwitch = pickerSource.indexOf(
      "await switchProject(",
      createFlush,
    );
    expect(createFlush).toBeGreaterThan(openSwitch);
    expect(createSwitch).toBeGreaterThan(createFlush);
  });
});
