// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";
import pickerSource from "./ProjectPicker.tsx?raw";
import {
  createButtonLabel,
  openButtonLabel,
  type BusyPhase,
} from "./projectPickerPhases";

describe("createButtonLabel", () => {
  it("walks the create flow: idle → preparing → restarting", () => {
    expect(createButtonLabel(null)).toBe("Create & open");
    expect(createButtonLabel("preparing")).toBe("Preparing project…");
    expect(createButtonLabel("restarting")).toBe("Restarting…");
  });

  it("surfaces the shared registry-remove phase", () => {
    expect(createButtonLabel("removing")).toBe("Removing…");
  });
});

describe("openButtonLabel", () => {
  it("only the restart phase changes the Open label", () => {
    const idle: BusyPhase | null = null;
    expect(openButtonLabel(idle)).toBe("Open");
    expect(openButtonLabel("preparing")).toBe("Open");
    expect(openButtonLabel("removing")).toBe("Open");
    expect(openButtonLabel("restarting")).toBe("Restarting…");
  });
});

describe("ProjectPicker phase wiring (source contract, review LOW 2)", () => {
  it("handleCreate walks preparing → createProject → restarting → switchProject", () => {
    // The create flow must show the phase walk in order — a reordered or
    // dropped step would show the wrong phase while the real work runs
    // (backlog 486955d5). Each search starts after the previous anchor so
    // handleOpen's earlier restarting/switchProject can't satisfy it.
    const preparingIdx = pickerSource.indexOf('setPhase("preparing")');
    const createIdx = pickerSource.indexOf("await createProject(", preparingIdx);
    const restartingIdx = pickerSource.indexOf(
      'setPhase("restarting")',
      createIdx,
    );
    const switchIdx = pickerSource.indexOf("await switchProject(", restartingIdx);
    expect(preparingIdx).toBeGreaterThan(-1);
    expect(createIdx).toBeGreaterThan(preparingIdx);
    expect(restartingIdx).toBeGreaterThan(createIdx);
    expect(switchIdx).toBeGreaterThan(restartingIdx);
  });

  it("every error/finally path resets the phase (no stuck-busy state)", () => {
    // handleOpen's catch + handleCreate's catch + handleRemove's finally —
    // a dropped reset re-introduces a stuck-busy picker (backlog 486955d5).
    const resets = pickerSource.match(/setPhase\(null\)/g) ?? [];
    expect(resets.length).toBe(3);
  });

  it("handleOpen enters the restarting phase before its switchProject", () => {
    const openFn = pickerSource.indexOf("async function handleOpen(");
    const restarting = pickerSource.indexOf('setPhase("restarting")', openFn);
    // handleOpen's OWN call — the `path` argument is unique to it
    // (handleCreate passes chosenPath), so handleCreate's later
    // restarting/switchProject cannot satisfy this guard when handleOpen's
    // own wiring is deleted or reordered (round-2 LOW 1, plan 969510fc).
    const switchCall = pickerSource.indexOf(
      "await switchProject(path)",
      restarting,
    );
    expect(openFn).toBeGreaterThan(-1);
    expect(restarting).toBeGreaterThan(openFn);
    expect(switchCall).toBeGreaterThan(restarting);
  });
});
