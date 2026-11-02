import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  MIN_LEAD_WIDTH,
  MIN_WORKBENCH_WIDTH,
  actions,
  app,
  clampWorkbenchWidth,
  maxWorkbenchWidth,
} from "./store";
import { fixtureState } from "../test/fixtures";

/** jsdom reads its viewport from `window.innerWidth`, so tests pin it explicitly. */
function setWindowWidth(width: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
}

describe("window-relative workbench width", () => {
  const originalInnerWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");

  beforeEach(() => {
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
});
