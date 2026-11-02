import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  actions,
  app,
  BOARD_DIVIDER_WIDTH,
  clampBoardWidth,
  defaultBoardWidth,
  MIN_BOARD_OUTPUT_WIDTH,
  MIN_BOARD_WIDTH,
  maxBoardWidth,
} from "./store";

const state = () => get(app);
const originalInnerWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");

function setWindowWidth(width: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
}

describe("mission control split", () => {
  beforeEach(() => {
    setWindowWidth(1600);
    app.update((current) => ({ ...current, boardWidth: null }));
  });

  afterEach(() => {
    if (originalInnerWidth) Object.defineProperty(window, "innerWidth", originalInnerWidth);
  });

  it("opens on today's proportion when nothing is saved", () => {
    expect(state().boardWidth).toBeNull();
    // Today's layout splits the free width 0.88fr / 1.12fr, i.e. 44% to the list.
    expect(defaultBoardWidth(1600)).toBe(704);
    expect(defaultBoardWidth(1024)).toBeLessThan(1024 / 2);
  });

  it("keeps the list above its status column and the output above its own minimum", () => {
    expect(MIN_BOARD_WIDTH).toBe(300);
    expect(MIN_BOARD_OUTPUT_WIDTH).toBe(420);
    expect(maxBoardWidth(1600)).toBe(1600 - 420 - BOARD_DIVIDER_WIDTH);
    expect(clampBoardWidth(200, 1600)).toBe(MIN_BOARD_WIDTH);
    expect(clampBoardWidth(2_000, 1600)).toBe(1600 - 420 - BOARD_DIVIDER_WIDTH);
    expect(clampBoardWidth(700, 1600)).toBe(700);
    // A window too narrow to hold both panes still floors the list, never the output.
    expect(clampBoardWidth(700, 700)).toBe(MIN_BOARD_WIDTH);
    expect(maxBoardWidth(700)).toBe(MIN_BOARD_WIDTH);
  });

  it("remembers a dragged width and resets to the default proportion", () => {
    actions.setBoardWidth(640);
    expect(state().boardWidth).toBe(640);
    actions.setBoardWidth(2_000);
    expect(state().boardWidth).toBe(1_171);
    actions.setBoardWidth(120);
    expect(state().boardWidth).toBe(300);
    actions.setBoardWidth(null);
    expect(state().boardWidth).toBeNull();
  });

  it("gives width back when the window shrinks and never widens the list on its own", () => {
    actions.setBoardWidth(1_100);
    expect(state().boardWidth).toBe(1_100);

    actions.reclampBoardWidth(1280);
    expect(state().boardWidth).toBe(1_280 - 420 - BOARD_DIVIDER_WIDTH);

    actions.reclampBoardWidth(1600);
    expect(state().boardWidth).toBe(1_280 - 420 - BOARD_DIVIDER_WIDTH);

    // A default proportion keeps adapting by itself, so there is nothing to clamp.
    actions.setBoardWidth(null);
    actions.reclampBoardWidth(800);
    expect(state().boardWidth).toBeNull();
  });
});
