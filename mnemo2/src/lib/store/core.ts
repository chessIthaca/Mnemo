import { derived, writable } from "svelte/store";
import { noExternalLead } from "../externalLeadEngine";
import { initialLeadRuntime } from "../leadState";
import { loadSoundToggles } from "../sounds";
import { emptyProjectUsage } from "../teamState";
import { logUi } from "../uiLog";
import type { AppState, ToastLevel, Worker } from "../types";

let sequence = 0;
export const nextId = (prefix: string) => `${prefix}-${++sequence}`;

export function leadError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * When this app instance booted, as the anchor for the Trace route's
 * "This session" time filter — the engine starts with the app, so its ring
 * holds nothing older.
 */
export const SESSION_START_MS = Date.now();

export function createInitialState(): AppState {
  return {
    route: "start",
    projectId: null,
    projects: [],
    workers: [],
    leadEntries: [],
    leadDraft: { text: "", images: [] },
    leadRuntime: initialLeadRuntime(),
    selectedWorkerId: null,
    teamPeekOpen: false,
    teamFilter: "all",
    teamSlots: 0,
    teamActive: 0,
    projectUsage: emptyProjectUsage(),
    workbenchOpen: false,
    workbenchTab: "browser",
    workbenchWidth: 480,
    /** No saved mission-control split until the user drags or the project restores one. */
    boardWidth: null,
    /** No Trace-route filter hand-over until an entry point plants one. */
    traceEntry: null,
    /** No plan hand-over until the Memory tab plants one. */
    planDocumentId: null,
    /** No file hand-over until the Memory tab plants one. */
    workbenchFile: null,
    workerPanelId: null,
    browserUrl: "about:blank",
    browserTitle: "",
    browserLoading: false,
    browserSupported: null,
    browserError: null,
    browserImage: null,
    plan: null,
    planItems: [],
    board: [],
    diff: null,
    diffFiles: [],
    diffWorkerId: null,
    diffLoading: false,
    diffError: null,
    projectConflict: null,
    indexing: null,
    externalLead: noExternalLead(),
    cleanupProjectId: null,
    cleanupBusy: false,
    workerDeletion: null,
    closeProjectPending: false,
    newProjectParent: null,
    creatingProject: false,
    toast: null,
  };
}

export const app = writable<AppState>(createInitialState());

// Notification-sound toggles ([ui] sound_*): read once at startup so the store
// can gate on them; the Appearance page pushes every later save through
// applySoundToggles, so a flipped toggle applies to the very next event.
void loadSoundToggles();

export const currentProject = derived(app, ($app) =>
  $app.projects.find((project) => project.id === $app.projectId) ?? null,
);

export const selectedWorker = derived(app, ($app) =>
  $app.workers.find((worker) => worker.id === $app.selectedWorkerId) ?? null,
);

export const needsYouCount = derived(
  app,
  ($app) => $app.workers.filter((worker) => worker.needsYou).length,
);

export function withToast(state: AppState, message: string, level: ToastLevel = "info"): AppState {
  logUi(level, message, { source: "toast", route: state.route });
  return { ...state, toast: { id: ++sequence, message, level } };
}

export function updateWorker(
  state: AppState,
  workerId: string,
  change: (worker: Worker) => Worker,
): AppState {
  return {
    ...state,
    workers: state.workers.map((worker) =>
      worker.id === workerId ? change(worker) : worker,
    ),
  };
}
