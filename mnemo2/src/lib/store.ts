import { get } from "svelte/store";
import { createProjectTransport, type ProjectRecord } from "./projectEngine";
import { probeMnemo2Host } from "./runtime";
import {
  applyExternalLeadEvent,
  createExternalLeadTransport,
  externalLeadFromStatus,
  noExternalLead,
  sameProjectPath,
  type ExternalLeadEvent,
} from "./externalLeadEngine";
import { applyStoredTheme } from "./theme";
import {
  createOpenWorkerTransport,
  openWorkerNotice,
  openWorkerOutcome,
  type OpenWorkerRequest,
} from "./workerSelection";
import { emptyProjectUsage } from "./teamState";
import { initialLeadRuntime } from "./leadState";
import type { Project, ToastLevel } from "./types";
// The `./store/*` imports come last on purpose: `core` creates `app` and reads
// the sound toggles at import time, exactly where store.ts used to do it
// inline, and `browser` installs the native-pane visibility subscription.
import { app, createInitialState, leadError, withToast } from "./store/core";
import { discardPendingPreferences, flushWorkbenchPreferences, hasPendingPreferences } from "./store/preferences";
import {
  browserBack,
  browserForward,
  browserOverlay,
  browserReload,
  connectBrowser,
  ensureBrowser,
  navigateBrowser,
  openBrowser,
  openExternalBrowser,
  setBrowserRect,
  setBrowserVisible,
} from "./store/browser";
import {
  closeScreenshot,
  closeWorkbench,
  disconnectWorkbench,
  openScreenshot,
  openWorkerDiff,
  openWorkbench,
  reclampBoardWidth,
  reclampWorkbenchWidth,
  refreshDiff,
  restoreWorkbench,
  selectDiffWorker,
  setBoardWidth,
  setWorkbenchTab,
  setWorkbenchWidth,
} from "./store/workbench";
import {
  clearPlanDocument,
  clearTraceEntry,
  clearWorkbenchFile,
  closeWorkerPanel,
  openCost,
  openFileInWorkbench,
  openGraph,
  openLead,
  openMemory,
  openPlanDocument,
  openPlans,
  openRepoPath,
  openTeam,
  openTrace,
  openWorkerPanel,
  selectWorker,
  setTeamFilter,
  toggleTeamPeek,
} from "./store/navigation";
import { connectBoard, disconnectBoard, syncBoard } from "./store/board";
import { connectIndexing, disconnectIndexing, syncIndexing } from "./store/indexing";
import { connectTeam, disconnectTeam, getTeamTransport, syncTeam } from "./store/team";
import {
  addLeadDraftImage,
  answerLead,
  approveLead,
  bumpLeadSession,
  clearLeadDraft,
  compactLead,
  connectNativeLead,
  interruptLead,
  removeLeadDraftImage,
  retryLead,
  sendLeadMessage,
  setLeadDraftText,
  setLeadModel,
  stopNativeLead,
} from "./store/lead";
import {
  cancelDeleteWorkers,
  compactWorker,
  confirmDeleteWorkers,
  fixFinding,
  mergeWorker,
  pauseWorker,
  recordWorkerCheck,
  replyWorker,
  requestDeleteOldWorkers,
  requestDeleteWorkers,
  resolveFinding,
  resumeWorker,
  revertMerge,
  reviewWorker,
  setWorkerModel,
  setWorkerSlots,
  spawnFixer,
  stopAll,
  stopWorker,
  wontfixFinding,
} from "./store/teamActions";

export { SESSION_START_MS, app, createInitialState, currentProject, needsYouCount, selectedWorker } from "./store/core";
export { MIN_LEAD_WIDTH, MIN_WORKBENCH_WIDTH, maxWorkbenchWidth, clampWorkbenchWidth, MIN_BOARD_WIDTH, MIN_BOARD_OUTPUT_WIDTH, BOARD_DIVIDER_WIDTH, maxBoardWidth, clampBoardWidth, defaultBoardWidth } from "./store/layout";
export { OLD_WORKER_STATUSES, isOldWorker } from "./store/teamActions";

/** App-level listeners for Claude Code's open requests and attach/detach events. */
let externalLeadUnlisten: (() => void)[] = [];
let externalLeadStart: Promise<void> | null = null;
let externalLeadToken = 0;
/** Counts attach/detach events, so a slower status read never overwrites a newer event. */
let externalLeadEvents = 0;
/** The listener for Claude Code asking to show a worker in mission control. */
let openWorkerUnlisten: (() => void) | null = null;
let openWorkerStart: Promise<void> | null = null;
let openWorkerToken = 0;

function projectFromRecord(record: ProjectRecord): Project {
  return {
    id: record.path,
    name: record.name,
    path: record.path,
    branch: record.branch ?? "",
    leadModel: record.lead_model ?? "",
    description: record.state_error ?? "",
    lastSession: record.last_session_ms
      ? new Date(record.last_session_ms).toLocaleString()
      : "No saved session",
    workerSlots: record.worker_slots ?? 3,
    available: record.available ?? true,
    unfinishedWorkers: record.unfinished_workers ?? 0,
    reviewWorkers: record.review_workers ?? record.unfinished_workers ?? 0,
  };
}

async function openProject(projectId: string, allowConflict = false): Promise<void> {
  const project = get(app).projects.find((item) => item.id === projectId);
  if (!project) {
    app.update((state) => withToast(state, "That project is not registered.", "error"));
    return;
  }
  if (hasPendingPreferences()) await flushWorkbenchPreferences();
  if (project.available === false) {
    app.update((state) => withToast(state, project.description || "The project folder is unavailable.", "error"));
    return;
  }
  const transport = createProjectTransport();
  if (transport) {
    try {
      if (!(await probeMnemo2Host())) {
        app.update((state) => withToast(state, "The native project host is unavailable.", "error"));
        return;
      }
      const attachment = await transport.attach(project.path, allowConflict);
      if (attachment.conflict_pid != null) {
        app.update((state) => ({
          ...state,
          projectConflict: {
            projectId,
            pid: attachment.conflict_pid!,
            startedAt: attachment.conflict_started_at,
          },
        }));
        return;
      }
    } catch (error) {
      app.update((state) => withToast(state, leadError(error), "error"));
      return;
    }
  }
  bumpLeadSession();
  await stopNativeLead();
  await disconnectTeam();
  await disconnectBoard();
  disconnectIndexing();
  await disconnectWorkbench();
  app.update((state) => ({
    ...state,
    board: [],
    route: "lead",
    projectId,
    projectConflict: null,
    indexing: null,
    externalLead: noExternalLead(),
    cleanupProjectId: null,
    workerDeletion: null,
    closeProjectPending: false,
    leadRuntime: initialLeadRuntime(),
    leadDraft: { text: "", images: [] },
    workers: [],
    leadEntries: [],
    plan: null,
    planItems: [],
    diff: null,
    diffFiles: [],
    diffWorkerId: null,
    diffLoading: false,
    diffError: null,
    selectedWorkerId: null,
    teamPeekOpen: false,
    teamSlots: 0,
    teamActive: 0,
    projectUsage: emptyProjectUsage(),
    workbenchOpen: false,
    workerPanelId: null,
    browserUrl: "about:blank",
    browserTitle: "",
    browserLoading: false,
    browserSupported: null,
    browserError: null,
    browserImage: null,
    toast: null,
  }));
  // Dependent startup IPC is rejected until the automatic lead open has
  // completed, so the lead opens first — no timers, retries, or redundant
  // user action. A genuine lead-open failure is already surfaced on the lead
  // runtime by connectNativeLead.
  await connectNativeLead(project, allowConflict);
  // A project switch during the lead open owns the rest of the startup.
  if (get(app).projectId !== projectId) return;
  await Promise.all([connectTeam(), connectBoard(), connectIndexing()]);
  // The runtime (and its indexing status) exists once the lead has opened.
  await syncIndexing();
  await syncExternalLead(project);
  await syncTeam();
  await restoreWorkbench(project);
}

async function loadProjects(): Promise<void> {
  const transport = createProjectTransport();
  if (!transport) return;
  // Only the real Mnemo2 host answers the handshake; Mnemo1's Browser tab is
  // a Tauri WebView too and its IPC never registered these commands.
  if (!(await probeMnemo2Host())) return;
  try {
    const records = await transport.list();
    app.update((state) => ({
      ...state,
      projects: records.map(projectFromRecord),
    }));
  } catch (error) {
    app.update((state) => withToast(state, `Could not load projects: ${leadError(error)}`, "error"));
  }
}

/** The open project's external lead as the native host reports it now. */
async function syncExternalLead(project: Project): Promise<void> {
  const transport = createExternalLeadTransport();
  if (!transport || !(await probeMnemo2Host())) return;
  const events = externalLeadEvents;
  try {
    const status = await transport.status();
    app.update((state) =>
      state.projectId === project.id && events === externalLeadEvents
        ? { ...state, externalLead: externalLeadFromStatus(status, project.path) }
        : state,
    );
  } catch {
    // A host without external-lead support: the built-in lead leads.
  }
}

function handleExternalLeadEvent(token: number, event: ExternalLeadEvent): void {
  if (token !== externalLeadToken) return;
  const state = get(app);
  const project = state.projects.find((item) => item.id === state.projectId);
  const externalLead = applyExternalLeadEvent(state.externalLead, event, project?.path ?? null);
  if (externalLead === state.externalLead) return;
  externalLeadEvents += 1;
  app.update((current) => ({ ...current, externalLead }));
}

/**
 * Tell the host why a project Claude Code asked for did not open, so the
 * waiting helper reports the reason instead of timing out. The window may be
 * hidden, so the toast alone would reach nobody.
 */
function reportExternalOpenFailure(root: string, reason: string): void {
  const transport = createExternalLeadTransport();
  void transport?.reportOpenFailure(root, reason).catch(() => undefined);
}

/**
 * Why `project` is not open and leading after `openProject`, or null when it
 * is. A failed lead open keeps the project selected and shows the error on
 * the lead runtime, so the project id alone is not success.
 */
function externalOpenFailure(project: Project): string | null {
  const after = get(app);
  if (after.projectId !== project.id) {
    const conflict = after.projectConflict;
    return conflict
      ? `another Mnemo2 process (pid ${conflict.pid}) is using the project`
      : after.toast?.message ?? "the project did not open";
  }
  const lead = after.leadRuntime;
  if (lead.status === "error") return lead.error ?? "the lead could not start";
  // Idle or still opening once the open has returned: no lead came up.
  return lead.status === "ready" ? null : "the native lead did not start";
}

/**
 * Open the registered project at `root` for Claude Code. An unregistered root
 * is never added here: the user adds projects themselves.
 */
async function openExternalRoot(root: string): Promise<void> {
  const find = () => get(app).projects.find((item) => sameProjectPath(item.path, root));
  let project = find();
  if (!project) {
    // The registry may have changed since the list was loaded.
    await loadProjects();
    project = find();
  }
  if (!project) {
    app.update((state) =>
      withToast(
        state,
        `Claude Code asked to open ${root}, which is not a Mnemo2 project. Add it first.`,
        "error",
      ),
    );
    reportExternalOpenFailure(root, "it is not a registered Mnemo2 project");
    return;
  }
  const { projectId: open, leadRuntime } = get(app);
  // A failed earlier open leaves the project selected with the error on its
  // lead: asking again is a retry (the cause may be fixed), not a no-op.
  if (open === project.id && leadRuntime.status !== "error") return;
  if (open !== null && open !== project.id) {
    // Switching would stop the open project's lead; the user decides that.
    const name = project.name;
    app.update((state) =>
      withToast(
        state,
        `Claude Code asked to open ${name}, but another project is open. Close it first.`,
        "error",
      ),
    );
    reportExternalOpenFailure(root, "another project is open in this Mnemo2 window");
    return;
  }
  await openProject(project.id);
  const failure = externalOpenFailure(project);
  if (failure) reportExternalOpenFailure(root, failure);
}

/**
 * Start listening for Claude Code's requests, then open the project the app
 * was started with (`--project`). Safe to call again; browser-only sessions
 * and hosts without the commands do nothing.
 */
function initExternalLead(): Promise<void> {
  if (externalLeadStart) return externalLeadStart;
  const transport = createExternalLeadTransport();
  if (!transport) return Promise.resolve();
  const token = ++externalLeadToken;
  const start = (async () => {
    try {
      if (!(await probeMnemo2Host())) return;
      // Listen before loading the registry so no early request is missed.
      const unlisten = await Promise.all([
        transport.onOpenRequest((request) => {
          if (token === externalLeadToken) void openExternalRoot(request.root);
        }),
        transport.onChanged((event) => handleExternalLeadEvent(token, event)),
      ]);
      if (token !== externalLeadToken) {
        for (const stop of unlisten) stop();
        return;
      }
      externalLeadUnlisten = unlisten;
      await loadProjects();
      const root = await transport.startupProject();
      if (root && token === externalLeadToken) await openExternalRoot(root);
    } catch {
      // A host without the external-lead commands has nothing to start.
    }
  })();
  externalLeadStart = start;
  return start;
}

function disconnectExternalLead(): void {
  externalLeadToken += 1;
  externalLeadStart = null;
  for (const unlisten of externalLeadUnlisten) unlisten();
  externalLeadUnlisten = [];
}

/**
 * Start listening for Claude Code asking to show a worker (the helper's
 * `show-worker`). Safe to call again; browser-only sessions and hosts without
 * the event have nothing to listen to.
 */
async function initOpenWorker(): Promise<void> {
  if (openWorkerStart) return openWorkerStart;
  const transport = createOpenWorkerTransport();
  if (!transport) return Promise.resolve();
  const token = ++openWorkerToken;
  const start = (async () => {
    try {
      // Like the external-lead listeners: Mnemo1's Browser tab is a Tauri
      // WebView too, and only the real Mnemo2 host emits this event.
      if (!(await probeMnemo2Host())) return;
      const unlisten = await transport.onOpenWorker((request) => {
        if (token === openWorkerToken) handleOpenWorkerRequest(request);
      });
      if (token !== openWorkerToken) {
        unlisten();
        return;
      }
      openWorkerUnlisten = unlisten;
    } catch {
      // A host without the open-worker event has nothing to show.
    }
  })();
  openWorkerStart = start;
  return start;
}

function disconnectOpenWorker(): void {
  openWorkerToken += 1;
  openWorkerStart = null;
  openWorkerUnlisten?.();
  openWorkerUnlisten = null;
}

/**
 * Show the worker Claude Code names: mission control opens on it, exactly like
 * a row click. A request that cannot be honored — another project's root, an
 * unknown worker, no project open — keeps the view and says so in a notice.
 */
function handleOpenWorkerRequest(request: OpenWorkerRequest): void {
  const state = get(app);
  const project = state.projects.find((item) => item.id === state.projectId) ?? null;
  const outcome = openWorkerOutcome(request, state.workers, project);
  if (outcome.kind === "select") {
    selectWorker(outcome.workerId);
    return;
  }
  app.update((current) => withToast(current, openWorkerNotice(outcome), "error"));
}

async function openFolder(): Promise<void> {
  const transport = createProjectTransport();
  if (!transport) {
    app.update((state) => withToast(state, "Opening a folder needs the native shell.", "error"));
    return;
  }
  // A Tauri WebView is not necessarily the Mnemo2 host: Mnemo1's Browser tab
  // has one too, and its IPC never registered this command (Tauri answers
  // "not allowed by ACL"). Confirm the handshake before invoking anything.
  if (!(await probeMnemo2Host())) {
    app.update((state) => withToast(state, "Opening a folder needs the native shell.", "error"));
    return;
  }
  try {
    const folder = await transport.pickFolder();
    if (!folder) return;
    const record = await transport.add(folder);
    await loadProjects();
    await openProject(record.path);
  } catch (error) {
    app.update((state) => withToast(state, `Could not open the folder: ${leadError(error)}`, "error"));
  }
}

async function closeProjectNow(): Promise<void> {
  if (hasPendingPreferences()) await flushWorkbenchPreferences();
  bumpLeadSession();
  await stopNativeLead();
  await disconnectTeam();
  await disconnectBoard();
  disconnectIndexing();
  await disconnectWorkbench();
  app.update((state) => ({
    ...state,
    board: [],
    route: "start",
    projectId: null,
    projectConflict: null,
    indexing: null,
    externalLead: noExternalLead(),
    cleanupProjectId: null,
    cleanupBusy: false,
    workerDeletion: null,
    closeProjectPending: false,
    newProjectParent: null,
    creatingProject: false,
    leadRuntime: initialLeadRuntime(),
    leadDraft: { text: "", images: [] },
    workers: [],
    selectedWorkerId: null,
    teamPeekOpen: false,
    teamSlots: 0,
    teamActive: 0,
    projectUsage: emptyProjectUsage(),
    workbenchOpen: false,
    workerPanelId: null,
  }));
}

async function closeProject(): Promise<void> {
  const state = get(app);
  if (state.closeProjectPending) return;
  const active = state.leadRuntime.busy || state.workers.some((worker) =>
    ["dispatching", "working", "fixing", "merging"].includes(worker.status),
  );
  if (active) {
    app.update((current) => ({ ...current, closeProjectPending: true }));
    return;
  }
  await closeProjectNow();
}

function cancelCloseProject(): void {
  app.update((state) => ({ ...state, closeProjectPending: false }));
}

async function confirmCloseProject(): Promise<void> {
  try {
    const transport = getTeamTransport();
    if (transport) await transport.stopAll();
    await closeProjectNow();
  } catch (error) {
    app.update((state) => ({
      ...withToast(state, `Could not stop the team: ${leadError(error)}`, "error"),
      closeProjectPending: false,
    }));
  }
}

async function reviewProject(projectId: string): Promise<void> {
  await openProject(projectId);
  if (get(app).projectId !== projectId) return;
  const worker = get(app).workers.find((item) => item.status !== "merged");
  if (!worker) {
    app.update((state) => withToast(state, "There are no unfinished worker branches to review."));
    return;
  }
  app.update((state) => ({ ...state, selectedWorkerId: worker.id }));
  openTeam();
}

function cancelProjectConflict(): void {
  app.update((state) => ({ ...state, projectConflict: null }));
}

async function confirmProjectConflict(): Promise<void> {
  const conflict = get(app).projectConflict;
  if (conflict) await openProject(conflict.projectId, true);
}

function requestCleanup(projectId: string): void {
  app.update((state) => ({ ...state, cleanupProjectId: projectId }));
}

function cancelCleanup(): void {
  if (!get(app).cleanupBusy) app.update((state) => ({ ...state, cleanupProjectId: null }));
}

async function confirmCleanup(): Promise<void> {
  const current = get(app);
  const project = current.projects.find((item) => item.id === current.cleanupProjectId);
  const transport = createProjectTransport();
  if (!project || !transport || current.cleanupBusy) return;
  app.update((state) => ({ ...state, cleanupBusy: true }));
  try {
    const report = await transport.cleanup(project.path);
    app.update((state) => withToast({
      ...state,
      cleanupProjectId: null,
      cleanupBusy: false,
    }, `Removed ${report.removed.length} clean worktree(s); retained ${report.skipped.length} for review.`));
    await loadProjects();
  } catch (error) {
    app.update((state) => ({
      ...withToast(state, `Cleanup failed: ${leadError(error)}`, "error"),
      cleanupBusy: false,
    }));
  }
}

async function openNewProject(): Promise<void> {
  const transport = createProjectTransport();
  if (!transport) {
    app.update((state) => withToast(state, "Project creation needs the native shell.", "error"));
    return;
  }
  try {
    if (!(await probeMnemo2Host())) {
      app.update((state) => withToast(state, "Project creation needs the native shell.", "error"));
      return;
    }
    const parent = await transport.pickFolder();
    if (parent) app.update((state) => ({ ...state, newProjectParent: parent }));
  } catch (error) {
    app.update((state) => withToast(state, leadError(error), "error"));
  }
}

async function createProject(name: string): Promise<void> {
  const parent = get(app).newProjectParent;
  if (!parent || get(app).creatingProject) return;
  app.update((state) => ({ ...state, creatingProject: true }));
  try {
    const transport = createProjectTransport();
    if (!transport) throw new Error("Project creation needs the native shell.");
    const record = await transport.create(parent, name);
    await loadProjects();
    app.update((state) => ({ ...state, newProjectParent: null }));
    await openProject(record.path);
  } catch (error) {
    app.update((state) => withToast(state, `Could not create the project: ${leadError(error)}`, "error"));
  } finally {
    app.update((state) => ({ ...state, creatingProject: false }));
  }
}

function cancelNewProject(): void {
  if (!get(app).creatingProject) app.update((state) => ({ ...state, newProjectParent: null }));
}

/** Surface a transient status or failure message to the user. */
function showToast(message: string, level: ToastLevel = "info"): void {
  app.update((state) => withToast(state, message, level));
}

function dismissToast() {
  app.update((state) => ({ ...state, toast: null }));
}

function resetDemo() {
  discardPendingPreferences();
  bumpLeadSession();
  void stopNativeLead();
  void disconnectTeam();
  void disconnectBoard();
  disconnectIndexing();
  disconnectExternalLead();
  disconnectOpenWorker();
  void disconnectWorkbench();
  app.set(createInitialState());
}

export const actions = {
  openProject,
  loadProjects,
  initExternalLead,
  initOpenWorker,
  openFolder,
  closeProject,
  reviewProject,
  cancelCloseProject,
  confirmCloseProject,
  cancelProjectConflict,
  confirmProjectConflict,
  requestCleanup,
  cancelCleanup,
  confirmCleanup,
  openNewProject,
  createProject,
  cancelNewProject,
  openLead,
  openTeam,
  openCost,
  openGraph,
  openPlans,
  openMemory,
  openTrace,
  clearTraceEntry,
  openPlanDocument,
  clearPlanDocument,
  openFileInWorkbench,
  clearWorkbenchFile,
  openRepoPath,
  toggleTeamPeek,
  selectWorker,
  setTeamFilter,
  openWorkerPanel,
  closeWorkerPanel,
  openWorkbench,
  closeWorkbench,
  setWorkbenchTab,
  setWorkbenchWidth,
  reclampWorkbenchWidth,
  refreshDiff,
  selectDiffWorker,
  openWorkerDiff,
  openBrowser,
  navigateBrowser,
  connectBrowser,
  ensureBrowser,
  setBrowserRect,
  setBrowserVisible,
  browserOverlay,
  browserBack,
  browserForward,
  browserReload,
  openExternalBrowser,
  openScreenshot,
  closeScreenshot,
  sendLeadMessage,
  setLeadDraftText,
  addLeadDraftImage,
  removeLeadDraftImage,
  clearLeadDraft,
  showToast,
  interruptLead,
  compactLead,
  approveLead,
  answerLead,
  setLeadModel,
  retryLead,
  replyWorker,
  stopWorker,
  pauseWorker,
  requestDeleteWorkers,
  requestDeleteOldWorkers,
  cancelDeleteWorkers,
  confirmDeleteWorkers,
  resumeWorker,
  setWorkerModel,
  compactWorker,
  syncTeam,
  syncBoard,
  reviewWorker,
  recordWorkerCheck,
  resolveFinding,
  fixFinding,
  spawnFixer,
  wontfixFinding,
  mergeWorker,
  revertMerge,
  stopAll,
  setWorkerSlots,
  dismissToast,
  resetDemo,
  /** Read the stored `[ui] theme` and paint it (see `lib/theme.ts`). */
  initTheme: applyStoredTheme,
  setBoardWidth,
  reclampBoardWidth,
};
