/** The narrowest the lead's conversation may get; the side panel yields the rest of the window. */
export const MIN_LEAD_WIDTH = 420;
/** The narrowest the workbench or the worker panel may be dragged to. */
export const MIN_WORKBENCH_WIDTH = 320;

/** The window width the side-panel bounds are measured against. */
export function viewportWidth(): number {
  return typeof window === "undefined" ? Number.POSITIVE_INFINITY : window.innerWidth;
}

/**
 * The widest a side panel may get in a window this wide: every pixel the lead
 * does not need, but never below the panel's own floor.
 */
export function maxWorkbenchWidth(windowWidth: number): number {
  return Math.max(MIN_WORKBENCH_WIDTH, windowWidth - MIN_LEAD_WIDTH);
}

/**
 * Keep a side-panel width between its floor and the window-relative ceiling.
 * Always a whole pixel: a scaled display hands the drag handlers fractional
 * coordinates, and the saved preference field is a `u32`.
 */
export function clampWorkbenchWidth(workbenchWidth: number, windowWidth: number): number {
  return Math.round(Math.min(maxWorkbenchWidth(windowWidth), Math.max(MIN_WORKBENCH_WIDTH, workbenchWidth)));
}

/**
 * The narrowest the mission-control worker list may be dragged to. The worker
 * rows keep a fixed 152px status column, so the list needs real room.
 */
export const MIN_BOARD_WIDTH = 300;
/** The narrowest the mission-control worker output may get; the list yields the rest. */
export const MIN_BOARD_OUTPUT_WIDTH = 420;
/** The draggable divider's own column between the list and the output, in CSS pixels. */
export const BOARD_DIVIDER_WIDTH = 9;
/** Share of the layout the worker list takes when nothing is saved (today's 0.88fr/1.12fr). */
const DEFAULT_BOARD_SHARE = 0.44;

/** The widest the worker list may get in a window this wide. */
export function maxBoardWidth(windowWidth: number): number {
  return Math.max(MIN_BOARD_WIDTH, windowWidth - MIN_BOARD_OUTPUT_WIDTH - BOARD_DIVIDER_WIDTH);
}

/**
 * Keep a worker-list width between its floor and the window-relative ceiling,
 * in whole pixels so the saved `u32` preference can never see a float.
 */
export function clampBoardWidth(boardWidth: number, windowWidth: number): number {
  return Math.round(Math.min(maxBoardWidth(windowWidth), Math.max(MIN_BOARD_WIDTH, boardWidth)));
}

/** The width a window this wide opens mission control with when nothing is saved. */
export function defaultBoardWidth(windowWidth: number): number {
  return clampBoardWidth(Math.round(windowWidth * DEFAULT_BOARD_SHARE), windowWidth);
}
