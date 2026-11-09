import { get } from "svelte/store";
import { createProjectTransport } from "../projectEngine";
import { probeMnemo2Host } from "../runtime";
import { createWorkbenchTransport, type WorkbenchTransport } from "../workbenchEngine";
import type { Project, WorkbenchTab } from "../types";
import { app, leadError, withToast } from "./core";
import { clampBoardWidth, clampWorkbenchWidth, viewportWidth } from "./layout";
import { schedulePreferenceSave } from "./preferences";
import { disconnectBrowser } from "./browser";

let workbenchTransport: WorkbenchTransport | null = null;
let diffRequest = 0;
let diffSyncTimer: ReturnType<typeof setTimeout> | null = null;

export async function disconnectWorkbench(): Promise<void> {
  diffRequest += 1;
  if (diffSyncTimer) {
    clearTimeout(diffSyncTimer);
    diffSyncTimer = null;
  }
  workbenchTransport = null;
  await disconnectBrowser();
}

export async function restoreWorkbench(project: Project): Promise<void> {
  const transport = createProjectTransport();
  if (!transport || !(await probeMnemo2Host())) return;
  try {
    const preferences = await transport.preferences();
    app.update((state) => state.projectId === project.id ? {
      ...state,
      workbenchOpen: preferences.workbench_open,
      workbenchTab: preferences.workbench_tab,
      workbenchWidth: preferences.workbench_width,
      workbenchWideWidth: preferences.workbench_wide_width ?? null,
      browserUrl: preferences.browser_url,
      browserImage: null,
      diffWorkerId: preferences.diff_worker_id,
      boardWidth: preferences.board_width,
      diff: null,
      diffFiles: [],
      diffError: null,
    } : state);
    if (preferences.workbench_open && preferences.workbench_tab === "diff") {
      void refreshDiff();
    }
  } catch (error) {
    app.update((state) => state.projectId === project.id
      ? withToast(state, `Could not restore workbench preferences: ${leadError(error)}`, "error")
      : state);
  }
}

export async function refreshDiff(): Promise<void> {
  const current = get(app);
  const workerId = current.diffWorkerId ?? current.selectedWorkerId ?? current.workers[0]?.id;
  if (!workerId) {
    diffRequest += 1;
    app.update((state) => ({
      ...state,
      diff: null,
      diffFiles: [],
      diffLoading: false,
      diffError: null,
    }));
    return;
  }
  const transport = workbenchTransport ?? createWorkbenchTransport();
  if (!transport) {
    app.update((state) => ({
      ...state,
      diffLoading: false,
      diffError: "Open Mnemo2's desktop app to read real worker diffs.",
    }));
    return;
  }
  workbenchTransport = transport;
  const request = ++diffRequest;
  const projectId = current.projectId;
  app.update((state) => ({ ...state, diffWorkerId: workerId, diffLoading: true, diffError: null }));
  try {
    const snapshot = await transport.diff(workerId);
    app.update((state) => {
      if (
        request !== diffRequest ||
        state.projectId !== projectId ||
        state.diffWorkerId !== workerId ||
        workbenchTransport !== transport
      ) return state;
      return {
        ...state,
        diff: snapshot,
        diffFiles: snapshot.files,
        diffLoading: false,
        workers: state.workers.map((worker) => worker.id === workerId ? {
          ...worker,
          files: snapshot.files.map((file) => file.path),
          diff: {
            files: snapshot.total_files,
            additions: snapshot.total_additions,
            deletions: snapshot.total_deletions,
          },
        } : worker),
      };
    });
  } catch (error) {
    app.update((state) => {
      if (request !== diffRequest || state.projectId !== projectId) return state;
      return {
        ...state,
        diff: null,
        diffFiles: [],
        diffLoading: false,
        diffError: leadError(error),
      };
    });
  }
}

export function scheduleDiffSync(): void {
  const state = get(app);
  if (!state.workbenchOpen || state.workbenchTab !== "diff" || diffSyncTimer) return;
  diffSyncTimer = setTimeout(() => {
    diffSyncTimer = null;
    void refreshDiff();
  }, 800);
}

export function selectDiffWorker(workerId: string): void {
  if (!get(app).workers.some((worker) => worker.id === workerId)) return;
  app.update((state) => ({
    ...state,
    diffWorkerId: workerId,
    diff: null,
    diffFiles: [],
    diffError: null,
  }));
  schedulePreferenceSave();
  void refreshDiff();
}

export function openWorkerDiff(workerId: string): void {
  if (!get(app).workers.some((worker) => worker.id === workerId)) return;
  app.update((state) => ({
    ...state,
    diffWorkerId: workerId,
    diff: null,
    diffFiles: [],
    diffError: null,
  }));
  openWorkbench("diff");
}

export function openWorkbench(workbenchTab: WorkbenchTab = "browser") {
  app.update((state) => ({
    ...state,
    teamPeekOpen: false,
    workbenchOpen: true,
    workerPanelId: null,
    workbenchTab,
  }));
  if (workbenchTab === "diff" && get(app).leadRuntime.mode === "native") void refreshDiff();
  schedulePreferenceSave();
}

export function closeWorkbench() {
  app.update((state) => ({ ...state, workbenchOpen: false }));
  schedulePreferenceSave();
}

export function setWorkbenchTab(workbenchTab: WorkbenchTab) {
  app.update((state) => ({ ...state, workbenchOpen: true, workerPanelId: null, workbenchTab }));
  if (workbenchTab === "diff" && get(app).leadRuntime.mode === "native") void refreshDiff();
  schedulePreferenceSave();
}

export function setWorkbenchWidth(workbenchWidth: number) {
  app.update((state) => ({
    ...state,
    workbenchWidth: clampWorkbenchWidth(workbenchWidth, viewportWidth()),
  }));
  schedulePreferenceSave();
}

/** Drag a wide panel tab; `null` goes back to following the window. */
export function setWideWorkbenchWidth(width: number | null) {
  app.update((state) => ({
    ...state,
    workbenchWideWidth: width === null ? null : clampWorkbenchWidth(width, viewportWidth()),
  }));
  schedulePreferenceSave();
}

/**
 * Re-clamp a saved or dragged width after the window shrank, so a wide panel
 * never squeezes the main tab below its minimum. No-op while the widths still
 * fit. A wide tab that follows the window stays null.
 */
export function reclampWorkbenchWidth(windowWidth: number = viewportWidth()) {
  let changed = false;
  app.update((state) => {
    const workbenchWidth = clampWorkbenchWidth(state.workbenchWidth, windowWidth);
    const workbenchWideWidth =
      state.workbenchWideWidth === null ? null : clampWorkbenchWidth(state.workbenchWideWidth, windowWidth);
    if (workbenchWidth === state.workbenchWidth && workbenchWideWidth === state.workbenchWideWidth) return state;
    changed = true;
    return { ...state, workbenchWidth, workbenchWideWidth };
  });
  if (changed) schedulePreferenceSave();
}

/** Drag the mission-control divider; `null` restores the default proportion. */
export function setBoardWidth(boardWidth: number | null) {
  app.update((state) => ({
    ...state,
    boardWidth: boardWidth === null ? null : clampBoardWidth(boardWidth, viewportWidth()),
  }));
  schedulePreferenceSave();
}

/**
 * Re-clamp a saved or dragged list width after the window shrank, so the worker
 * output never falls below its minimum. A default proportion adapts by itself.
 */
export function reclampBoardWidth(windowWidth: number = viewportWidth()) {
  let changed = false;
  app.update((state) => {
    if (state.boardWidth === null) return state;
    const boardWidth = clampBoardWidth(state.boardWidth, windowWidth);
    if (boardWidth === state.boardWidth) return state;
    changed = true;
    return { ...state, boardWidth };
  });
  if (changed) schedulePreferenceSave();
}

export async function openScreenshot(workerId: string, path: string): Promise<void> {
  const transport = workbenchTransport ?? createWorkbenchTransport();
  if (!transport) {
    app.update((state) => ({ ...state, browserError: "Screenshots need the native shell." }));
    return;
  }
  workbenchTransport = transport;
  app.update((state) => ({
    ...state,
    workbenchOpen: true,
    workbenchTab: "browser",
    browserImage: { dataUrl: "", label: path.split("/").pop() ?? "Worker screenshot" },
    browserLoading: true,
    browserError: null,
  }));
  try {
    const dataUrl = await transport.screenshot(workerId, path);
    app.update((state) => ({
      ...state,
      browserImage: state.browserImage ? { ...state.browserImage, dataUrl } : null,
      browserLoading: false,
    }));
  } catch (error) {
    app.update((state) => ({
      ...state,
      browserImage: null,
      browserLoading: false,
      browserError: leadError(error),
    }));
  }
}

export function closeScreenshot(): void {
  app.update((state) => ({ ...state, browserImage: null }));
}
