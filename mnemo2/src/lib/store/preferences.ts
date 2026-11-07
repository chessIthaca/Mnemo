import { get } from "svelte/store";
import { createProjectTransport, type ProjectPreferences, type ProjectTransport } from "../projectEngine";
import { app, leadError, withToast } from "./core";

let preferenceSave: {
  projectId: string;
  workspaceRoot: string;
  preferences: ProjectPreferences;
  transport: ProjectTransport;
} | null = null;
let preferenceTimer: ReturnType<typeof setTimeout> | null = null;

export async function flushWorkbenchPreferences(): Promise<void> {
  if (preferenceTimer) {
    clearTimeout(preferenceTimer);
    preferenceTimer = null;
  }
  const pending = preferenceSave;
  preferenceSave = null;
  if (!pending) return;
  try {
    await pending.transport.savePreferences(pending.workspaceRoot, pending.preferences);
  } catch (error) {
    app.update((state) => state.projectId === pending.projectId
      ? withToast(state, `Could not save workbench preferences: ${leadError(error)}`, "error")
      : state);
  }
}

export function schedulePreferenceSave(): void {
  const state = get(app);
  const transport = createProjectTransport();
  const project = state.projects.find((item) => item.id === state.projectId);
  if (!transport || !project) return;
  preferenceSave = {
    projectId: project.id,
    workspaceRoot: project.path,
    transport,
    preferences: {
      workbench_open: state.workbenchOpen,
      workbench_tab: state.workbenchTab,
      workbench_width: Math.round(state.workbenchWidth),
      browser_url: state.browserUrl,
      diff_worker_id: state.diffWorkerId,
      board_width: state.boardWidth === null ? null : Math.round(state.boardWidth),
    },
  };
  if (preferenceTimer) clearTimeout(preferenceTimer);
  preferenceTimer = setTimeout(() => {
    preferenceTimer = null;
    void flushWorkbenchPreferences();
  }, 200);
}

/** Whether a workbench-preference save is queued but not flushed yet. */
export function hasPendingPreferences(): boolean {
  return preferenceSave !== null;
}

/** Drop a queued preference save without saving it (demo reset teardown). */
export function discardPendingPreferences(): void {
  preferenceSave = null;
  if (preferenceTimer) {
    clearTimeout(preferenceTimer);
    preferenceTimer = null;
  }
}
