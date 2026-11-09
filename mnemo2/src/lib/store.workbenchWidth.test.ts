import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProjectPreferences } from "./projectEngine";
import {
  MIN_LEAD_WIDTH,
  MIN_WORKBENCH_WIDTH,
  actions,
  app,
  clampBoardWidth,
  clampWorkbenchWidth,
  defaultWideWorkbenchWidth,
  maxWorkbenchWidth,
  workbenchPanelWidth,
} from "./store";

import { fixtureState } from "../test/fixtures";

const { transport } = vi.hoisted(() => ({
  transport: { savePreferences: vi.fn() },
}));
vi.mock("./projectEngine", () => ({ createProjectTransport: () => transport }));

/** jsdom reads its viewport from `window.innerWidth`, so tests pin it explicitly. */
function setWindowWidth(width: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
}

describe("window-relative workbench width", () => {
  const originalInnerWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");

  beforeEach(() => {
    transport.savePreferences.mockReset();
    app.set(fixtureState());
    setWindowWidth(1600);
  });

  afterEach(() => {
    if (originalInnerWidth) Object.defineProperty(window, "innerWidth", originalInnerWidth);
  });

  it("lets the panel take every pixel the lead does not need", () => {
    expect(MIN_LEAD_WIDTH).toBe(420);
    expect(maxWorkbenchWidth(1600)).toBe(1180);

    actions.setWorkbenchWidth(1180);
    expect(get(app).workbenchWidth).toBe(1180);

    // Beyond the ceiling the drag stops with the lead still at its minimum.
    actions.setWorkbenchWidth(1500);
    expect(get(app).workbenchWidth).toBe(1180);
  });

  it("keeps the panel's own floor when the window cannot hold both", () => {
    setWindowWidth(600);
    expect(maxWorkbenchWidth(600)).toBe(MIN_WORKBENCH_WIDTH);
    expect(clampWorkbenchWidth(900, 600)).toBe(MIN_WORKBENCH_WIDTH);

    actions.setWorkbenchWidth(900);
    expect(get(app).workbenchWidth).toBe(MIN_WORKBENCH_WIDTH);
  });

  it("re-clamps a wide panel when the window shrinks, never widening it back", () => {
    actions.setWorkbenchWidth(1100);
    expect(get(app).workbenchWidth).toBe(1100);

    actions.reclampWorkbenchWidth(1280);
    expect(get(app).workbenchWidth).toBe(860);

    // Growing the window again leaves the panel where the user last had it.
    actions.reclampWorkbenchWidth(1600);
    expect(get(app).workbenchWidth).toBe(860);
  });

  it("applies the same bounds to keyboard resizing", () => {
    actions.setWorkbenchWidth(1180);
    actions.setWorkbenchWidth(1180 + 24);
    expect(get(app).workbenchWidth).toBe(1180);

    actions.setWorkbenchWidth(MIN_WORKBENCH_WIDTH);
    actions.setWorkbenchWidth(MIN_WORKBENCH_WIDTH - 24);
    expect(get(app).workbenchWidth).toBe(MIN_WORKBENCH_WIDTH);
  });

  it("panel_widths_are_saved_as_whole_pixels", async () => {
    // On a scaled display a drag hands the store a fractional pointer
    // coordinate (`window.innerWidth - event.clientX`), but the Rust
    // preference struct types both widths as u32: one float rejects the
    // whole save with "invalid type: floating point ..., expected u32".
    expect(clampWorkbenchWidth(697.3333740234375, 1600)).toBe(697);
    expect(clampBoardWidth(640.6, 1600)).toBe(641);

    actions.setWorkbenchWidth(697.3333740234375);
    actions.setBoardWidth(640.6);
    expect(get(app).workbenchWidth).toBe(697);
    expect(get(app).boardWidth).toBe(641);

    await vi.waitFor(() => expect(transport.savePreferences).toHaveBeenCalled());
    const dragged = transport.savePreferences.mock.lastCall?.[1] as ProjectPreferences;
    expect(dragged.workbench_width).toBe(697);
    expect(dragged.board_width).toBe(641);

    // A width that reaches the store by another path (a restore, or a future
    // setter) is rounded once more where the preference payload is built.
    transport.savePreferences.mockClear();
    app.update((state) => ({ ...state, workbenchWidth: 697.4, boardWidth: 640.6 }));
    actions.closeWorkbench();
    await vi.waitFor(() => expect(transport.savePreferences).toHaveBeenCalled());
    const rescued = transport.savePreferences.mock.lastCall?.[1] as ProjectPreferences;
    expect(rescued.workbench_width).toBe(697);
    expect(rescued.board_width).toBe(641);
  });
});

describe("wide panel tabs", () => {
  const originalInnerWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");

  beforeEach(() => {
    transport.savePreferences.mockReset();
    app.set(fixtureState());
    setWindowWidth(1600);
  });

  afterEach(() => {
    if (originalInnerWidth) Object.defineProperty(window, "innerWidth", originalInnerWidth);
  });

  it("opens at 62% of the window until dragged", () => {
    expect(defaultWideWorkbenchWidth(1600)).toBe(992);
    // The main tab keeps its floor, so the share yields to the panel bounds.
    expect(defaultWideWorkbenchWidth(1000)).toBe(580);
    expect(defaultWideWorkbenchWidth(600)).toBe(MIN_WORKBENCH_WIDTH);
  });

  it("keeps a drag on one kind of tab out of the other's width", () => {
    expect(workbenchPanelWidth("diff", 480, 1000, 1600)).toBe(480);
    expect(workbenchPanelWidth("trace", 480, null, 1600)).toBe(992);
    expect(workbenchPanelWidth("graph", 480, 1000, 1280)).toBe(860);
  });

  it("stores the dragged wide width as whole pixels inside the panel bounds", () => {
    actions.setWideWorkbenchWidth(697.3);
    expect(get(app).workbenchWideWidth).toBe(697);

    actions.setWideWorkbenchWidth(5000);
    expect(get(app).workbenchWideWidth).toBe(1180);

    actions.setWideWorkbenchWidth(null);
    expect(get(app).workbenchWideWidth).toBeNull();
  });

  it("re-clamps a saved wide width on a shrunk window and leaves a following one null", () => {
    actions.setWideWorkbenchWidth(1100);
    actions.reclampWorkbenchWidth(1280);
    expect(get(app).workbenchWideWidth).toBe(860);

    actions.setWideWorkbenchWidth(null);
    actions.reclampWorkbenchWidth(1280);
    expect(get(app).workbenchWideWidth).toBeNull();
  });

  it("saves the wide width rounded, and null when it follows the window", async () => {
    actions.setWideWorkbenchWidth(697.4);
    await vi.waitFor(() => expect(transport.savePreferences).toHaveBeenCalled());
    const dragged = transport.savePreferences.mock.lastCall?.[1] as ProjectPreferences;
    expect(dragged.workbench_wide_width).toBe(697);

    transport.savePreferences.mockClear();
    actions.setWideWorkbenchWidth(null);
    await vi.waitFor(() => expect(transport.savePreferences).toHaveBeenCalled());
    const reset = transport.savePreferences.mock.lastCall?.[1] as ProjectPreferences;
    expect(reset.workbench_wide_width).toBeNull();
  });
});
