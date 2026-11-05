import { derived, get, writable } from "svelte/store";
import { createLeadTransport, type LeadApprovalDecision, type LeadTransport } from "./leadEngine";
import { createProjectTransport, type ProjectPreferences, type ProjectRecord, type ProjectTransport } from "./projectEngine";
import type { RepoPathRef } from "./repoPaths";
import { probeMnemo2Host } from "./runtime";
import { createStreamCoalescer } from "./streamCoalescer";
import {
  createTeamTransport,
  type TeamTransport,
  type WorkerDeletionProgress,
} from "./teamEngine";
import { createBoardTransport, type BoardTransport } from "./boardEngine";
import { createIndexingTransport, newerIndexing, type IndexingSnapshot, type IndexingTransport } from "./indexingEngine";
import {
  applyExternalLeadEvent,
  createExternalLeadTransport,
  externalLeadFromStatus,
  noExternalLead,
  sameProjectPath,
  type ExternalLeadEvent,
} from "./externalLeadEngine";
import { boardFromSnapshot } from "./boardState";
import { createWorkbenchTransport, type WorkbenchTransport } from "./workbenchEngine";
import { createBrowserTransport, type BrowserTransport } from "./browserEngine";
import { workerScreenshotPaths } from "./workbenchState";
import {
  applyModelSwitch,
  applyWorkerLiveEvent,
  emptyWorkerLive,
  STREAM_ONLY_EVENTS,
  workerThreadFromRecord,
  type WorkerLive,
} from "./workerThread";
import { applyWorkerEvent, applyWorkerStatus } from "./workerActivity";
import { leadEventSound, loadSoundToggles, notifySound, workerTransitionSound } from "./sounds";
import { applyStoredTheme } from "./theme";
import type { TraceEntry } from "./trace";
import { logUi } from "./uiLog";
import {
  createOpenWorkerTransport,
  openWorkerNotice,
  openWorkerOutcome,
  type OpenWorkerRequest,
} from "./workerSelection";
import {
  deletionDiscardsWork,
  deletionPreviewFromWire,
  emptyProjectUsage,
  findingFromWire,
  findingToWire,
  inboxFromWire,
  projectUsageFrom,
  reviewEvidenceFrom,
  reviewEvidenceToWire,
  reviewRiskFrom,
  waitsOnUser,
  workerDisplayName,
  workerKindFrom,
  type TeamEvent,
  type TeamMergeRecord,
  type TeamMessage,
  type TeamWorkerRecord,
} from "./teamState";
import {
  applyLeadEvent,
  applyLeadSnapshot,
  initialLeadRuntime,
  leadEventAffectsUsage,
  leadEventNeedsSnapshot,
} from "./leadState";
import { insertActivityCard, type ActivityCard } from "./teamActivity";
import type {
  AppState,
  Check,
  Finding,
  LeadAnswerInput,
  LeadEntry,
  LeadEvent,
  MergeRecord,
  Project,
  ReviewEvidence,
  TeamFilter,
  ToastLevel,
  Worker,
  WorkerActivity,
  WorkerStatus,
  WorkbenchTab,
} from "./types";

let sequence = 0;
const nextId = (prefix: string) => `${prefix}-${++sequence}`;

let leadTransport: LeadTransport | null = null;
let leadUnlisten: (() => void) | null = null;
let leadSessionToken = 0;

let teamTransport: TeamTransport | null = null;
let teamUnlisten: (() => void) | null = null;
let teamToken = 0;
let teamSyncTimer: ReturnType<typeof setTimeout> | null = null;
let boardTransport: BoardTransport | null = null;
let boardUnlisten: (() => void) | null = null;
let boardToken = 0;
let boardSyncTimer: ReturnType<typeof setTimeout> | null = null;
let indexingTransport: IndexingTransport | null = null;
let indexingUnlisten: (() => void) | null = null;
let indexingToken = 0;
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
let workbenchTransport: WorkbenchTransport | null = null;
let diffRequest = 0;
let diffSyncTimer: ReturnType<typeof setTimeout> | null = null;
let browserTransport: BrowserTransport | null = null;
let browserUnlisten: (() => void)[] = [];
let browserToken = 0;
/** Orders native visibility requests (see `browser_set_visible`). */
let browserVisibilitySeq = 0;
/** Whether the mounted Browser tab wants its native pane on screen. */
let browserTabWantsPane = false;
/** The visibility last sent to the native pane. */
let browserPaneShown = false;
let preferenceSave: {
  projectId: string;
  workspaceRoot: string;
  preferences: ProjectPreferences;
  transport: ProjectTransport;
} | null = null;
let preferenceTimer: ReturnType<typeof setTimeout> | null = null;

function leadError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

async function disconnectWorkbench(): Promise<void> {
  diffRequest += 1;
  if (diffSyncTimer) {
    clearTimeout(diffSyncTimer);
    diffSyncTimer = null;
  }
  workbenchTransport = null;
  browserToken += 1;
  for (const unlisten of browserUnlisten) unlisten();
  browserUnlisten = [];
  const browser = browserTransport;
  browserTransport = null;
  browserTabWantsPane = false;
  browserPaneShown = false;
  if (browser) await browser.close().catch(() => {});
}

async function stopNativeLead(): Promise<void> {
  const unlisten = leadUnlisten;
  leadUnlisten = null;
  unlisten?.();
  const transport = leadTransport;
  leadTransport = null;
  if (transport) {
    try {
      await transport.close();
    } catch {
      // Closing a replaced lead is best effort; the next open owns the state.
    }
  }
}

async function refreshNativeLead(token: number): Promise<void> {
  const transport = leadTransport;
  if (!transport) return;
  try {
    const snapshot = await transport.snapshot();
    if (token === leadSessionToken) {
      leadStream.flush();
      app.update((state) => applyLeadSnapshot(state, snapshot));
    }
  } catch (error) {
    if (token === leadSessionToken) {
      app.update((state) => ({
        ...state,
        leadRuntime: { ...state.leadRuntime, error: leadError(error) },
      }));
    }
  }
}

/**
 * Lead events pass through a coalescer: while the window paints they apply one
 * by one, but a backlog from a hidden or throttled window lands in one update
 * (see streamCoalescer.ts). Flush it before any other write to lead state so
 * queued events never apply on top of a newer snapshot.
 */
const leadStream = createStreamCoalescer<{ token: number; event: LeadEvent }>(applyNativeLeadEvents);

function handleNativeLeadEvent(token: number, event: LeadEvent): void {
  if (token !== leadSessionToken) return;
  leadStream.push({ token, event });
}

function applyNativeLeadEvents(items: { token: number; event: LeadEvent }[]): void {
  const token = leadSessionToken;
  const events = items.filter((item) => item.token === token).map((item) => item.event);
  if (events.length === 0) return;
  app.update((state) => events.reduce(applyLeadEvent, state));
  // The lead's own turn-finished / needs-you / stopped-with-errors events ring
  // the matching notification sound (gated by the [ui] toggles, per-sound
  // debounce, more urgent sounds always through).
  const runtime = get(app).leadRuntime;
  let affectsUsage = false;
  let needsSnapshot = false;
  for (const event of events) {
    if (runtime.mode === "native" && runtime.generation === event.generation) {
      const sound = leadEventSound(event.event?.event);
      if (sound) notifySound(sound);
    }
    affectsUsage ||= leadEventAffectsUsage(event);
    needsSnapshot ||= leadEventNeedsSnapshot(event);
  }
  if (affectsUsage) {
    // The durable per-project accumulator changed; re-read the team snapshot
    // (debounced and project-guarded) so the header cost follows the lead too.
    scheduleTeamSync();
  }
  if (needsSnapshot) {
    setTimeout(() => {
      if (token === leadSessionToken) void refreshNativeLead(token);
    }, 25);
  }
}

async function connectNativeLead(project: Project, allowConflict = false): Promise<void> {
  const token = ++leadSessionToken;
  const transport = createLeadTransport();
  if (!transport) return;
  const available = await transport.probe().catch(() => false);
  if (!available || token !== leadSessionToken) return;
  await stopNativeLead();
  if (token !== leadSessionToken) return;
  leadTransport = transport;
  app.update((state) => ({
    ...state,
    leadRuntime: {
      ...initialLeadRuntime(),
      mode: "native",
      status: "opening",
    },
  }));
  try {
    const unlisten = await transport.onEvent((event) =>
      handleNativeLeadEvent(token, event),
    );
    if (token !== leadSessionToken) {
      unlisten();
      return;
    }
    leadUnlisten = unlisten;
    const snapshot = await transport.open({
      workspaceRoot: project.path,
      ...(allowConflict ? { allowInstanceConflict: true } : {}),
    });
    if (token !== leadSessionToken) return;
    leadStream.flush();
    app.update((state) => applyLeadSnapshot(state, snapshot));
  } catch (error) {
    if (token !== leadSessionToken) return;
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        status: "error",
        error: leadError(error),
      },
    }));
  }
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

/**
 * Whether the layout leaves room for the native Browser pane. The pane is an
 * OS window above the whole UI, so it may only show on the open Browser tab
 * of the Lead view with no drawer, sheet, or preview over it.
 */
function browserPaneAllowed(state: AppState): boolean {
  return (
    state.route === "lead" &&
    state.workbenchOpen &&
    state.workbenchTab === "browser" &&
    !state.teamPeekOpen &&
    !state.browserImage
  );
}

/** Send the pane's desired visibility as a new, ordered request. */
function pushBrowserVisibility(): Promise<void> {
  const wanted = browserTabWantsPane && browserPaneAllowed(get(app));
  browserPaneShown = wanted;
  const seq = ++browserVisibilitySeq;
  return browserCommand((transport) => transport.setVisible(wanted, seq));
}

// The layout, not a component's mount/unmount, has the final say: any change
// that covers the Browser tab (another tab, the team drawer, the Team view,
// a closed workbench) hides the pane, and uncovering it shows it again.
app.subscribe((state) => {
  if (!browserTransport) return;
  if ((browserTabWantsPane && browserPaneAllowed(state)) !== browserPaneShown) {
    void pushBrowserVisibility();
  }
});

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

function withToast(state: AppState, message: string, level: ToastLevel = "info"): AppState {
  logUi(level, message, { source: "toast", route: state.route });
  return { ...state, toast: { id: ++sequence, message, level } };
}

function updateWorker(
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

async function flushWorkbenchPreferences(): Promise<void> {
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

function schedulePreferenceSave(): void {
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

async function restoreWorkbench(project: Project): Promise<void> {
  const transport = createProjectTransport();
  if (!transport || !(await probeMnemo2Host())) return;
  try {
    const preferences = await transport.preferences();
    app.update((state) => state.projectId === project.id ? {
      ...state,
      workbenchOpen: preferences.workbench_open,
      workbenchTab: preferences.workbench_tab,
      workbenchWidth: preferences.workbench_width,
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
  if (preferenceSave) await flushWorkbenchPreferences();
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
  leadSessionToken += 1;
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
  if (preferenceSave) await flushWorkbenchPreferences();
  leadSessionToken += 1;
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
    if (teamTransport) await teamTransport.stopAll();
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

function openLead() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "lead" : state.route,
    teamPeekOpen: false,
  }));
}

function openTeam() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "team" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
  }));
}

/** Show the project's spend and savings; needs an open project. */
function openCost() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "cost" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
  }));
}

/** Show the project's code graph; needs an open project. */
function openGraph() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "graph" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
  }));
}

/** Show every board plan with its expandable details; needs an open project. */
function openPlans() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "plans" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
  }));
}

/** Show the project's knowledge records and memories; needs an open project. */
function openMemory() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "memory" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
  }));
}

/**
 * Open the Trace route — every model call, in detail. Optional filters carry
 * an entry point's hand-over (a worker from mission control, a lead turn
 * from the chat); the view consumes them once and they never enter a URL.
 */
function openTrace(filters?: TraceEntry) {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "trace" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
    traceEntry: filters ?? null,
  }));
}

/** Clear a consumed Trace hand-over so returning to the route starts unfiltered. */
function clearTraceEntry() {
  app.update((state) => (state.traceEntry === null ? state : { ...state, traceEntry: null }));
}

/**
 * Open a plan document: the Memory tab's link to `.coding/plans/<id>.md`. The
 * Plans route expands the plan, reads its document and scrolls it into view;
 * an id the board does not hold shows a short not-found note.
 */
function openPlanDocument(planId: string) {
  const id = planId.trim();
  if (id === "") return;
  app.update((state) => ({
    ...state,
    route: state.projectId ? "plans" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
    planDocumentId: id,
  }));
}

/** Clear a consumed plan hand-over so returning to the route starts collapsed. */
function clearPlanDocument() {
  app.update((state) => (state.planDocumentId === null ? state : { ...state, planDocumentId: null }));
}

/**
 * Open a project file in the Workbench: the Memory tab's link to a path a
 * record names. The Files tab reads it through its own `openPath` and reveals
 * `line` when the reference carries one.
 */
function openFileInWorkbench(path: string, line: number | null = null) {
  const target = path.trim();
  if (target === "") return;
  const at = line !== null && Number.isInteger(line) && line > 0 ? line : null;
  openWorkbench("files");
  app.update((state) => ({ ...state, workbenchFile: { path: target, line: at } }));
}

/** Clear a consumed file hand-over so returning to the tab opens the tree. */
function clearWorkbenchFile() {
  app.update((state) => (state.workbenchFile === null ? state : { ...state, workbenchFile: null }));
}

/**
 * Open a repository path a Markdown view names: a plan document goes to the
 * Plans route, anything else to the Workbench's Files tab at its line. This is
 * the one action every Markdown surface passes as `onOpenPath`.
 */
function openRepoPath(ref: RepoPathRef) {
  if (ref.kind === "plan" && ref.planId !== null) openPlanDocument(ref.planId);
  else openFileInWorkbench(ref.path, ref.line);
}

function toggleTeamPeek() {
  app.update((state) => ({ ...state, teamPeekOpen: !state.teamPeekOpen }));
}

function selectWorker(workerId: string) {
  app.update((state) => {
    if (!state.workers.some((worker) => worker.id === workerId)) return state;
    return {
      ...state,
      route: "team",
      selectedWorkerId: workerId,
      teamPeekOpen: false,
      workbenchOpen: false,
    };
  });
}

function setTeamFilter(teamFilter: TeamFilter) {
  app.update((state) => ({ ...state, teamFilter }));
}

/**
 * Stream a worker's output beside the lead chat. The route stays on "lead",
 * the Workbench gives way, and unknown ids are ignored.
 */
function openWorkerPanel(workerId: string) {
  app.update((state) => {
    if (!state.workers.some((worker) => worker.id === workerId)) return state;
    return {
      ...state,
      route: state.projectId ? "lead" : state.route,
      teamPeekOpen: false,
      workbenchOpen: false,
      workerPanelId: workerId,
    };
  });
}

/** Close the lead-side worker output panel; the Workbench stays closed. */
function closeWorkerPanel() {
  app.update((state) => ({ ...state, workerPanelId: null }));
}

async function refreshDiff(): Promise<void> {
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

function scheduleDiffSync(): void {
  const state = get(app);
  if (!state.workbenchOpen || state.workbenchTab !== "diff" || diffSyncTimer) return;
  diffSyncTimer = setTimeout(() => {
    diffSyncTimer = null;
    void refreshDiff();
  }, 800);
}

function selectDiffWorker(workerId: string): void {
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

function openWorkerDiff(workerId: string): void {
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

function openWorkbench(workbenchTab: WorkbenchTab = "browser") {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "lead" : state.route,
    teamPeekOpen: false,
    workbenchOpen: true,
    workerPanelId: null,
    workbenchTab,
  }));
  if (workbenchTab === "diff" && get(app).leadRuntime.mode === "native") void refreshDiff();
  schedulePreferenceSave();
}

function closeWorkbench() {
  app.update((state) => ({ ...state, workbenchOpen: false }));
  schedulePreferenceSave();
}

function setWorkbenchTab(workbenchTab: WorkbenchTab) {
  app.update((state) => ({ ...state, workbenchOpen: true, workerPanelId: null, workbenchTab }));
  if (workbenchTab === "diff" && get(app).leadRuntime.mode === "native") void refreshDiff();
  schedulePreferenceSave();
}

/** The narrowest the lead's conversation may get; the side panel yields the rest of the window. */
export const MIN_LEAD_WIDTH = 420;
/** The narrowest the workbench or the worker panel may be dragged to. */
export const MIN_WORKBENCH_WIDTH = 320;

/** The window width the side-panel bounds are measured against. */
function viewportWidth(): number {
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

function setWorkbenchWidth(workbenchWidth: number) {
  app.update((state) => ({
    ...state,
    workbenchWidth: clampWorkbenchWidth(workbenchWidth, viewportWidth()),
  }));
  schedulePreferenceSave();
}

/**
 * Re-clamp a saved or dragged width after the window shrank, so a wide panel
 * never squeezes the lead below its minimum. No-op while the width still fits.
 */
function reclampWorkbenchWidth(windowWidth: number = viewportWidth()) {
  let changed = false;
  app.update((state) => {
    const workbenchWidth = clampWorkbenchWidth(state.workbenchWidth, windowWidth);
    if (workbenchWidth === state.workbenchWidth) return state;
    changed = true;
    return { ...state, workbenchWidth };
  });
  if (changed) schedulePreferenceSave();
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

/** Drag the mission-control divider; `null` restores the default proportion. */
function setBoardWidth(boardWidth: number | null) {
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
function reclampBoardWidth(windowWidth: number = viewportWidth()) {
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

function approximateBrowserUrl(value: string): string {
  if (/^https?:\/\//i.test(value)) return value;
  if (/^(?:localhost|127\.0\.0\.1|\[::1\])(?::|\/|$)/i.test(value)) return `http://${value}`;
  return `https://${value}`;
}

function applyBrowserSnapshot(snapshot: { supported: boolean; url: string; title: string; loading: boolean }): void {
  app.update((state) => ({
    ...state,
    browserSupported: snapshot.supported,
    browserUrl: snapshot.url,
    browserTitle: snapshot.title,
    browserLoading: snapshot.loading,
  }));
  schedulePreferenceSave();
}

async function connectBrowser(): Promise<BrowserTransport | null> {
  if (browserTransport) return browserTransport;
  const transport = createBrowserTransport();
  if (!transport) return null;
  const token = ++browserToken;
  if (!(await probeMnemo2Host()) || token !== browserToken) return null;
  browserTransport = transport;
  try {
    const [supported, snapshot, unlisten, openUnlisten] = await Promise.all([
      transport.supported(),
      transport.snapshot(),
      transport.onEvent((event) => {
        if (token === browserToken) applyBrowserSnapshot(event);
      }),
      transport.onOpenRequest((event) => {
        if (token === browserToken) void navigateBrowser(event.url);
      }),
    ]);
    if (token !== browserToken) {
      unlisten();
      openUnlisten();
      return null;
    }
    browserUnlisten = [unlisten, openUnlisten];
    app.update((state) => ({
      ...state,
      browserSupported: supported,
      browserUrl: snapshot.url === "about:blank" ? state.browserUrl : snapshot.url,
      browserTitle: snapshot.title,
      browserLoading: snapshot.loading,
      browserError: null,
    }));
    return transport;
  } catch (error) {
    if (token === browserToken) {
      browserTransport = null;
      app.update((state) => ({ ...state, browserError: leadError(error) }));
    }
    return null;
  }
}

async function browserCommand(operation: (transport: BrowserTransport) => Promise<unknown>): Promise<void> {
  const transport = await connectBrowser();
  if (!transport) return;
  try {
    await operation(transport);
  } catch (error) {
    app.update((state) => ({ ...state, browserError: leadError(error) }));
  }
}

function ensureBrowser(rect: Parameters<BrowserTransport["ensure"]>[0], url: string): Promise<void> {
  return browserCommand(async (transport) => {
    await transport.ensure(rect, url);
  });
}

function setBrowserRect(rect: Parameters<BrowserTransport["setRect"]>[0]): Promise<void> {
  return browserCommand((transport) => transport.setRect(rect));
}

function setBrowserVisible(visible: boolean): Promise<void> {
  browserTabWantsPane = visible;
  return pushBrowserVisibility();
}

function browserOverlay(enter: boolean): void {
  void browserCommand((transport) => transport.overlay(enter));
}

function browserBack(): void {
  void browserCommand((transport) => transport.history("back"));
}

function browserForward(): void {
  void browserCommand((transport) => transport.history("forward"));
}

function browserReload(): void {
  void browserCommand((transport) => transport.reload());
}

function openExternalBrowser(): void {
  const url = get(app).browserUrl;
  if (url === "about:blank") return;
  if (!createBrowserTransport()) {
    window.open(url, "_blank", "noopener,noreferrer");
    return;
  }
  void browserCommand((transport) => transport.openExternal(url));
}

function openBrowser(url: string): void {
  void navigateBrowser(url);
}

async function navigateBrowser(url: string): Promise<void> {
  const value = url.trim();
  if (!value) return;
  app.update((state) => ({
    ...state,
    route: state.projectId ? "lead" : state.route,
    browserUrl: approximateBrowserUrl(value),
    browserTitle: "",
    browserImage: null,
    browserError: null,
    workbenchOpen: true,
    workbenchTab: "browser",
  }));
  schedulePreferenceSave();
  const transport = await connectBrowser();
  if (!transport) return;
  try {
    await transport.navigate(value);
  } catch (error) {
    app.update((state) => ({ ...state, browserError: leadError(error) }));
  }
}

async function openScreenshot(workerId: string, path: string): Promise<void> {
  const transport = workbenchTransport ?? createWorkbenchTransport();
  if (!transport) {
    app.update((state) => ({ ...state, browserError: "Screenshots need the native shell." }));
    return;
  }
  workbenchTransport = transport;
  app.update((state) => ({
    ...state,
    route: state.projectId ? "lead" : state.route,
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

function closeScreenshot(): void {
  app.update((state) => ({ ...state, browserImage: null }));
}

/**
 * Send one submission to the native lead.
 *
 * Resolves `true` only when the host accepted it. A rejected steer keeps the
 * turn's running state (busy, elapsed clock, streaming entry) intact — the
 * engine may still be working — and a rejected idle prompt becomes an error
 * instead of a fabricated idle turn. Either failure rolls back the optimistic
 * transcript entry and surfaces a toast.
 */
async function sendNativeLeadMessage(message: string, images: string[]): Promise<boolean> {
  const transport = leadTransport;
  if (!transport) return false;
  const token = leadSessionToken;
  // The optimistic entry goes after every event the host already sent.
  leadStream.flush();
  const before = get(app).leadRuntime;
  // A submission on a busy turn steers the running turn instead of starting
  // a new one: the elapsed clock and the streaming entry must survive.
  const steering = before.busy;
  const entryId = nextId("user");
  app.update((state) => ({
    ...state,
    leadEntries: [
      ...state.leadEntries,
      {
        id: entryId,
        kind: "message",
        author: "user",
        body: message,
        ...(images.length > 0 ? { images } : {}),
        time: "now",
      },
    ],
    leadRuntime: {
      ...state.leadRuntime,
      status: "ready",
      busy: true,
      error: null,
      errorRetrying: false,
      turnStartedAtMs: steering ? state.leadRuntime.turnStartedAtMs : Date.now(),
      streamingEntryId: steering ? state.leadRuntime.streamingEntryId : null,
    },
  }));
  try {
    await transport.prompt(message, images);
    return true;
  } catch (error) {
    if (token === leadSessionToken) {
      const failure = leadError(error);
      app.update((state) => {
        // Roll back the optimistic entry unless a later turn update followed
        // it, so a rejected submission is never shown as delivered.
        const entries = state.leadEntries.at(-1)?.id === entryId
          ? state.leadEntries.slice(0, -1)
          : state.leadEntries;
        const runtime = steering
          ? // The turn was already running: never fake idle after a rejected
            // steer, and leave busy/clock/streaming exactly as the engine set
            // them (a `finished` event during the await still wins).
            { ...state.leadRuntime, error: null, errorRetrying: false }
          : {
              ...state.leadRuntime,
              status: "error" as const,
              busy: false,
              error: failure,
              errorRetrying: false,
              turnStartedAtMs: before.turnStartedAtMs,
              streamingEntryId: before.streamingEntryId,
            };
        return withToast({ ...state, leadEntries: entries, leadRuntime: runtime }, failure, "error");
      });
    }
    return false;
  }
}

/**
 * Submit text (and optional image data URLs) to the lead.
 *
 * Resolves `true` when the submission was accepted (a prompt when idle, a
 * steer while busy) and `false` when it was refused. The unsent draft is
 * owned by the caller, so a refusal never discards it.
 */
async function sendLeadMessage(body: string, images: string[] = []): Promise<boolean> {
  const message = body.trim();
  const attachments = images.filter(
    (image) => typeof image === "string" && image.length > 0,
  );
  // An image-only submission is still a real message for a vision model.
  if (!message && attachments.length === 0) return false;
  const state = get(app);
  if (state.leadRuntime.mode === "native") {
    if (!leadTransport) {
      app.update((current) => withToast(current, "The native lead is not connected.", "error"));
      return false;
    }
    if (state.leadRuntime.status !== "ready") {
      app.update((current) => withToast(current, "The lead is still connecting."));
      return false;
    }
    // A pending question or approval owns the turn; answer it first. This is
    // the race-safe gate: a pending interaction that appeared after the
    // composer's own check still refuses the send and keeps the draft.
    if (state.leadRuntime.pending) {
      app.update((current) =>
        withToast(current, "Answer the lead's question before sending."),
      );
      return false;
    }
    return sendNativeLeadMessage(message, attachments);
  }
  // Browser-only Vite sessions have no engine behind them. Keep the typed
  // message, but never fabricate a lead reply.
  app.update((current) => ({
    ...withToast(
      current,
      "This build has no native shell; open Mnemo2's desktop app to run the lead.",
      "error",
    ),
    leadEntries: [
      ...current.leadEntries,
      {
        id: nextId("user"),
        kind: "message",
        author: "user",
        body: message,
        ...(attachments.length > 0 ? { images: attachments } : {}),
        time: "now",
      },
    ],
  }));
  return true;
}

/** Replace the unsent composer text, keeping any attached images. */
function setLeadDraftText(text: string): void {
  app.update((state) =>
    state.leadDraft.text === text
      ? state
      : { ...state, leadDraft: { ...state.leadDraft, text } },
  );
}

/** Attach a downscaled image data URL to the unsent draft. */
function addLeadDraftImage(dataUrl: string): void {
  if (!dataUrl) return;
  app.update((state) => ({
    ...state,
    leadDraft: {
      ...state.leadDraft,
      images: [...state.leadDraft.images, dataUrl],
    },
  }));
}

/** Detach one attached image by its position in the draft. */
function removeLeadDraftImage(index: number): void {
  app.update((state) => {
    if (index < 0 || index >= state.leadDraft.images.length) return state;
    return {
      ...state,
      leadDraft: {
        ...state.leadDraft,
        images: state.leadDraft.images.filter((_, position) => position !== index),
      },
    };
  });
}

/** Drop the whole unsent draft (after a send or on a project switch). */
function clearLeadDraft(): void {
  app.update((state) =>
    state.leadDraft.text === "" && state.leadDraft.images.length === 0
      ? state
      : { ...state, leadDraft: { text: "", images: [] } },
  );
}

/** Surface a transient status or failure message to the user. */
function showToast(message: string, level: ToastLevel = "info"): void {
  app.update((state) => withToast(state, message, level));
}

async function interruptLead(): Promise<void> {
  const transport = leadTransport;
  if (!transport) {
    const error = new Error("The native lead is not connected, so it cannot be interrupted.");
    app.update((state) => withToast(state, error.message, "error"));
    throw error;
  }
  const token = leadSessionToken;
  try {
    await transport.interrupt();
  } catch (error) {
    if (token === leadSessionToken) {
      app.update((state) => withToast(state, leadError(error), "error"));
    }
    throw error;
  }
}

/** Compact the lead's context window between turns; a failure is toasted. */
async function compactLead(): Promise<void> {
  const transport = leadTransport;
  if (!transport) {
    app.update((state) =>
      withToast(
        state,
        "The native lead is not connected, so its context cannot be compacted.",
        "error",
      ),
    );
    return;
  }
  const token = leadSessionToken;
  try {
    await transport.compact();
  } catch (error) {
    if (token === leadSessionToken) {
      app.update((state) => withToast(state, leadError(error), "error"));
    }
  }
}

function approveLead(requestId: string, decision: LeadApprovalDecision) {
  const transport = leadTransport;
  if (!transport) return;
  const token = leadSessionToken;
  void transport
    .approve(requestId, decision)
    .then(() => refreshNativeLead(token))
    .catch((error) => {
      if (token === leadSessionToken) {
        app.update((state) => withToast(state, leadError(error), "error"));
      }
    });
}

/**
 * Answer the pending interaction.
 *
 * Resolves `true` once the host accepted (and the answer landed), `false`
 * when it was refused. The caller owns the composer draft, so a refusal is
 * toasted here and must leave the typed answer intact.
 */
async function answerLead(
  questionId: string,
  answer: LeadAnswerInput,
): Promise<boolean> {
  const transport = leadTransport;
  if (!transport) return false;
  const token = leadSessionToken;
  try {
    await transport.answer(questionId, answer);
  } catch (error) {
    if (token === leadSessionToken) {
      app.update((state) => withToast(state, leadError(error), "error"));
    }
    return false;
  }
  // The answer landed; refresh the accepted snapshot in the background.
  void refreshNativeLead(token);
  return true;
}

function setLeadModel(spec: string, reasoningEffort: string | null = null) {
  const transport = leadTransport;
  if (!transport) return;
  const state = get(app);
  if (state.leadRuntime.busy) {
    app.update((current) =>
      withToast(current, "Stop the current turn before changing models."),
    );
    return;
  }
  const token = leadSessionToken;
  void transport
    .setModel(spec, reasoningEffort)
    .then((snapshot) => {
      if (token === leadSessionToken) {
        leadStream.flush();
        app.update((current) => applyLeadSnapshot(current, snapshot));
      }
    })
    .catch((error) => {
      if (token === leadSessionToken) {
        app.update((current) => withToast(current, leadError(error), "error"));
      }
    });
}

function retryLead() {
  const state = get(app);
  const project = state.projects.find((item) => item.id === state.projectId);
  if (!project) return;
  leadSessionToken += 1;
  void stopNativeLead();
  void connectNativeLead(project);
}

function requireTeam(): TeamTransport | null {
  if (!teamTransport) {
    app.update((state) => withToast(state, "The native team runtime is not connected.", "error"));
    return null;
  }
  return teamTransport;
}

function noteTeamError(error: unknown): void {
  app.update((state) => withToast(state, leadError(error), "error"));
}

function replyWorker(workerId: string, body: string) {
  const message = body.trim();
  if (!message) return;
  const transport = requireTeam();
  if (!transport) return;
  void transport
    .message(workerId, message)
    .then(() => syncTeam())
    .catch(noteTeamError);
}

function steerWorker(workerId: string) {
  replyWorker(
    workerId,
    "Steer: keep the current scope, avoid unrelated edits, and surface any conflict before continuing.",
  );
}

function stopWorker(workerId: string) {
  const transport = requireTeam();
  if (!transport) return;
  void transport
    .stop(workerId)
    .then(() => syncTeam())
    .catch(noteTeamError);
}

/** Statuses of workers whose task is over — what "Delete old workers" offers. */
export const OLD_WORKER_STATUSES: WorkerStatus[] = ["merged", "cancelled", "stopped", "interrupted", "failed"];

/**
 * Whether "Delete old workers" offers this worker: its task is over. That
 * is any terminal status, plus finished audit workers — read-only reviewers
 * are never merged, so they end in `completed`. A `completed` implementation
 * worker still awaits review and is never offered here.
 */
export function isOldWorker(worker: Pick<Worker, "status" | "kind">): boolean {
  return OLD_WORKER_STATUSES.includes(worker.status) ||
    (worker.kind === "audit" && worker.status === "completed");
}

/** A token that invalidates promises and events of superseded deletion
 * flows: each open request bumps it, so a canceled or reopened sheet never
 * applies a stale result. */
let deletionFlow = 0;

/** The streamed per-worker deletion progress listener; null while none is
 * subscribed. One listener serves the open request and is removed when
 * the sheet closes. */
let deletionUnlisten: (() => void) | null = null;

/** Merge one streamed deletion update into the open sheet's rows. */
function applyDeletionProgress(event: WorkerDeletionProgress): void {
  app.update((state) => {
    const request = state.workerDeletion;
    if (!request) return state;
    return {
      ...state,
      workerDeletion: {
        ...request,
        rows: request.rows.map((row) => {
          if (row.workerId !== event.worker_id) return row;
          return {
            ...row,
            name: event.name || row.name,
            preview: event.preview
              ? deletionPreviewFromWire(event.preview)
              : row.preview,
            error: event.error ?? row.error,
            deleted: event.phase === "deleting" && event.error === null,
          };
        }),
      },
    };
  });
}

/** Close the sheet, invalidating anything its flow still has in flight. */
function closeDeletionSheet(): void {
  deletionFlow += 1;
  deletionUnlisten?.();
  deletionUnlisten = null;
  app.update((state) => (state.workerDeletion ? { ...state, workerDeletion: null } : state));
}

/**
 * Open the deletion sheet at once: rows start in a checking state and fill
 * in per worker as the host streams finished previews
 * (`mnemo2://worker-deletion`), so slow Git checks never delay the
 * dialog. A request that is already open is ignored — the button cannot
 * open the sheet twice.
 */
async function requestDeleteWorkers(workerIds: string[]): Promise<void> {
  const transport = requireTeam();
  if (!transport || workerIds.length === 0) return;
  if (get(app).workerDeletion) return;
  const ids = [...new Set(workerIds)];
  const workers = get(app).workers;
  app.update((state) => ({
    ...state,
    workerDeletion: {
      phase: "checking",
      rows: ids.map((workerId) => ({
        workerId,
        name: workers.find((worker) => worker.id === workerId)?.name ?? workerId,
        preview: null,
        error: null,
        deleted: false,
      })),
    },
  }));
  const flow = ++deletionFlow;
  deletionUnlisten?.();
  deletionUnlisten = transport.onDeletionEvent
    ? await transport.onDeletionEvent(applyDeletionProgress)
    : null;
  try {
    const previews = (await transport.deletePreview(ids)).map(deletionPreviewFromWire);
    if (flow !== deletionFlow) return;
    app.update((state) => {
      const request = state.workerDeletion;
      if (!request) return state;
      return {
        ...state,
        workerDeletion: {
          ...request,
          rows: request.rows.map((row) => ({
            ...row,
            preview:
              previews.find((preview) => preview.worker_id === row.workerId) ?? row.preview,
          })),
          phase: "ready",
        },
      };
    });
  } catch (error) {
    if (flow !== deletionFlow) return;
    closeDeletionSheet();
    noteTeamError(error);
  }
}

/** Offer to delete every worker whose task is over. */
async function requestDeleteOldWorkers(): Promise<void> {
  const ids = get(app).workers.filter(isOldWorker).map((worker) => worker.id);
  if (ids.length === 0) {
    app.update((state) => withToast(state, "There are no old workers to delete."));
    return;
  }
  await requestDeleteWorkers(ids);
}

function cancelDeleteWorkers(): void {
  const request = get(app).workerDeletion;
  if (!request || request.phase === "deleting") return;
  closeDeletionSheet();
}

/**
 * Delete the confirmed workers; blocked ones are left in place. Results
 * stream in per worker; when some fail the sheet stays open with a retry
 * for exactly those workers, and closes only once nothing failed.
 */
async function confirmDeleteWorkers(): Promise<void> {
  const transport = requireTeam();
  const request = get(app).workerDeletion;
  if (!transport || !request || request.phase !== "ready") return;
  // After a failed round, retry only the workers that failed.
  const retrying = request.rows.some((row) => row.error !== null && !row.deleted);
  const rows = retrying
    ? request.rows.filter((row) => row.error !== null && !row.deleted)
    : request.rows.filter((row) => row.preview && row.preview.blocked === null);
  if (rows.length === 0) return;
  const discardWork = rows.some((row) => row.preview && deletionDiscardsWork(row.preview));
  const flow = deletionFlow;
  app.update((state) =>
    state.workerDeletion
      ? { ...state, workerDeletion: { ...state.workerDeletion, phase: "deleting" } }
      : state,
  );
  try {
    const outcome = await transport.deleteWorkers(
      rows.map((row) => row.workerId),
      discardWork,
    );
    if (flow !== deletionFlow) return;
    app.update((state) => {
      const current = state.workerDeletion;
      if (!current || current.phase !== "deleting") return state;
      const nextRows = current.rows.map((row) => {
        const result = outcome.results.find((entry) => entry.worker_id === row.workerId);
        if (!result) return row;
        return {
          ...row,
          name: result.name || row.name,
          error: result.error ?? row.error,
          deleted: result.error === null,
        };
      });
      const failed = nextRows.filter((row) => row.error !== null && !row.deleted);
      if (failed.length === 0) {
        return withToast(
          { ...state, workerDeletion: null },
          `Deleted ${rows.length} worker${rows.length === 1 ? "" : "s"} and ${rows.length === 1 ? "its" : "their"} history.`,
        );
      }
      // Keep the sheet open: the failed rows show their errors, with a
      // retry for exactly those workers.
      return { ...state, workerDeletion: { phase: "ready", rows: nextRows } };
    });
  } catch (error) {
    if (flow !== deletionFlow) return;
    // The whole command failed: keep the sheet so the user can retry.
    app.update((state) =>
      withToast(
        state.workerDeletion
          ? { ...state, workerDeletion: { ...state.workerDeletion, phase: "ready" } }
          : state,
        `Delete failed: ${leadError(error)}`,
        "error",
      ),
    );
  }
  await syncTeam();
  if (!get(app).workerDeletion) closeDeletionSheet();
}

function pauseWorker(workerId: string) {
  const transport = requireTeam();
  if (!transport) return;
  void transport
    .pause(workerId)
    .then(() => syncTeam())
    .catch(noteTeamError);
}

function resumeWorker(workerId: string) {
  const transport = requireTeam();
  if (!transport) return;
  void transport
    .resume(workerId)
    .then(() => syncTeam())
    .catch(noteTeamError);
}

/**
 * Move a worker to another model. A worker mid-run (working, dispatching,
 * fixing) keeps its session running: the switch applies to its next request
 * and its thread notes the change; every other worker comes back idle
 * with its transcript, as before.
 */
async function setWorkerModel(workerId: string, model: string, reasoningEffort: string | null): Promise<void> {
  const transport = requireTeam();
  if (!transport) return;
  let switched = false;
  try {
    const record = await transport.setModel(workerId, model, reasoningEffort);
    // A still-running record means the switch was applied live — the request
    // in flight keeps serving on the old model.
    switched = ["working", "dispatching", "fixing"].includes(record.status);
    app.update((state) => withToast(
      state,
      switched
        ? `${workerDisplayName(record.id, record.name)} keeps working; it runs on ${model} from its next call.`
        : `${workerDisplayName(record.id, record.name)} now runs on ${model}. Message it to continue.`,
    ));
  } catch (error) {
    noteTeamError(error);
  }
  await syncTeam();
  if (switched) noteWorkerModelSwitch(workerId, model);
}

/**
 * Note a live model switch in the worker's open thread tail. The note rides
 * the live entries (the saved transcript never holds it), stays in step with
 * the current saved generation, and flips to "Now running on …" once the
 * kernel's model_changed event reports the new model serving.
 */
function noteWorkerModelSwitch(workerId: string, model: string): void {
  const saved = workerSaved.get(workerId);
  if (!saved) return;
  let live = workerLive.get(workerId);
  if (!live || live.generation !== saved.generation || live.baseLength !== saved.length) {
    live = emptyWorkerLive(saved.generation, saved.length);
  }
  workerLive.set(workerId, applyModelSwitch(live, model));
  const thread = composeThread(workerId);
  if (!thread) return;
  app.update((state) => ({
    ...state,
    workers: state.workers.map((worker) =>
      worker.id === workerId ? { ...worker, messages: thread } : worker,
    ),
  }));
}

async function compactWorker(workerId: string): Promise<void> {
  await performTeam((transport) => transport.compact(workerId));
}

async function performTeam(
  operation: (transport: TeamTransport) => Promise<unknown>,
): Promise<void> {
  const transport = requireTeam();
  if (!transport) return;
  try {
    await operation(transport);
    await syncTeam();
  } catch (error) {
    noteTeamError(error);
  }
}

async function reviewWorker(
  workerId: string,
  summary: string,
  findings: Finding[],
  evidence: ReviewEvidence,
): Promise<void> {
  await performTeam((transport) =>
    transport.review(workerId, summary, findings.map(findingToWire), reviewEvidenceToWire(evidence)),
  );
}

async function recordWorkerCheck(
  workerId: string,
  name: string,
  status: "passed" | "failed",
  detail?: string | null,
): Promise<void> {
  await performTeam((transport) =>
    transport.recordCheck(workerId, name, status, detail ?? null),
  );
}

async function resolveFinding(
  workerId: string,
  findingId: string,
  status: "fixed" | "wontfix",
  evidence?: string | null,
  justification?: string | null,
): Promise<void> {
  await performTeam((transport) =>
    transport.resolveFinding(
      workerId,
      findingId,
      status,
      evidence ?? null,
      justification ?? null,
    ),
  );
}

async function fixFinding(
  workerId: string,
  findingId: string,
  oldString: string,
  newString: string,
): Promise<void> {
  const finding = get(app).workers
    .find((worker) => worker.id === workerId)
    ?.findings.find((item) => item.id === findingId);
  if (!finding) {
    app.update((state) => withToast(state, `Unknown finding '${findingId}'.`, "error"));
    return;
  }
  await performTeam((transport) =>
    transport.reviewFix(workerId, findingId, finding.file, oldString, newString),
  );
}

async function spawnFixer(workerId: string, findingId: string): Promise<void> {
  await performTeam((transport) => transport.routeFix(workerId, findingId));
}

async function wontfixFinding(
  workerId: string,
  findingId: string,
  justification: string,
): Promise<void> {
  await resolveFinding(workerId, findingId, "wontfix", null, justification);
}

async function mergeWorker(workerId: string): Promise<void> {
  await performTeam((transport) => transport.merge(workerId));
}

async function revertMerge(workerId: string): Promise<void> {
  await performTeam((transport) => transport.revertMerge(workerId));
}

async function stopAll(): Promise<void> {
  await performTeam((transport) => transport.stopAll());
}

function timeFromMs(milliseconds: number): string {
  if (!Number.isFinite(milliseconds) || milliseconds <= 0) return "unknown";
  return new Date(milliseconds).toLocaleTimeString();
}

function elapsedFromMs(milliseconds: number): string {
  if (!Number.isFinite(milliseconds) || milliseconds <= 0) return "—";
  const seconds = Math.floor(milliseconds / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

function mergeFromRecord(worker: TeamWorkerRecord, merge: TeamMergeRecord): MergeRecord {
  return {
    id: `merge-${worker.id}-${merge.commit}`,
    workerId: worker.id,
    commit: merge.commit,
    branch: merge.branch,
    target: merge.target,
    files: merge.files,
    additions: merge.additions,
    deletions: merge.deletions,
    checks: merge.checks,
    reviewSummary: merge.review_summary,
    model: merge.worker_model ?? worker.model_id ?? worker.model ?? "",
    tokens: merge.worker_tokens,
    cost: merge.cost_micros == null ? null : merge.cost_micros / 1_000_000,
    time: timeFromMs(merge.merged_at_ms),
    revertCommit: merge.revert_commit ?? undefined,
  };
}


function workerTimeline(record: TeamWorkerRecord): Worker["timeline"] {
  const time = (milliseconds: number) => milliseconds > 0 ? timeFromMs(milliseconds) : undefined;
  const timeline: Worker["timeline"] = [{
    id: `${record.id}-created`,
    label: "Task dispatched",
    detail: record.brief,
    status: "done",
    time: time(record.created_at_ms),
  }];
  if (record.turns > 0 || record.last_activity_ms > 0) {
    timeline.push({
      id: `${record.id}-activity`,
      label: "Worker activity",
      detail: `${record.turns} completed turn(s) · ${record.total_tokens} cumulative tokens`,
      status: ["working", "dispatching", "fixing"].includes(record.status) ? "active" : "done",
      time: time(record.last_activity_ms),
    });
  }
  for (const check of record.checks) {
    timeline.push({
      id: `${record.id}-check-${check.name}`,
      label: check.name,
      detail: check.detail || `Exact worker tip ${check.tip}`,
      status: check.status === "passed" ? "done" : check.status === "failed" ? "failed" : "active",
      time: time(record.last_activity_ms),
    });
  }
  if (record.review) {
    timeline.push({
      id: `${record.id}-review-${record.review.reviewed_at_ms}`,
      label: "Lead review recorded",
      detail: record.review.summary,
      status: record.findings.some((finding) => finding.status === "open") ? "active" : "done",
      time: time(record.review.reviewed_at_ms),
    });
  }
  if (record.merge) {
    timeline.push({
      id: `${record.id}-merge-${record.merge.commit}`,
      label: record.merge.revert_commit ? "Merge reverted" : `Landed in ${record.merge.target}`,
      detail: record.merge.revert_commit ?? record.merge.commit,
      status: "done",
      time: time(record.merge.merged_at_ms),
    });
  }
  if (record.status === "cancelled") {
    timeline.push({
      id: `${record.id}-cleaned`,
      label: "Checkout cleaned",
      detail: record.error ?? "The branch is retained for review.",
      status: "done",
    });
  } else if (["failed", "conflict", "needs_you"].includes(record.status)) {
    timeline.push({
      id: `${record.id}-attention`,
      label: "Needs attention",
      detail: record.error ?? "The worker stopped before finishing; open its thread for its last message.",
      status: "failed",
      time: time(record.last_activity_ms),
    });
  } else if (["stopped", "interrupted"].includes(record.status)) {
    timeline.push({
      id: `${record.id}-stopped`,
      label: "Session stopped",
      detail: "The branch and worktree are retained.",
      status: "done",
      time: time(record.last_activity_ms),
    });
  }
  return timeline;
}

function workerFromRecord(
  record: TeamWorkerRecord,
  index: number,
  messages: TeamMessage[],
): Worker {
  const status = record.status as WorkerStatus;
  // Fold the status snapshot into the live activity: needs_you waits on the
  // human, dispatching has not streamed yet, and non-running statuses clear.
  const activity = applyWorkerStatus(workerActivity.get(record.id) ?? null, status, Date.now());
  if (activity) workerActivity.set(record.id, activity);
  else workerActivity.delete(record.id);
  const progress: Record<WorkerStatus, number> = {
    dispatching: 10,
    working: 45,
    needs_you: 45,
    idle: 50,
    paused: 45,
    interrupted: 45,
    completed: 70,
    stopped: 70,
    in_review: 85,
    fixing: 75,
    merging: 95,
    merged: 100,
    conflict: 85,
    failed: 70,
    cancelled: 0,
  };
  return {
    id: record.id,
    name: workerDisplayName(record.id, record.name),
    initials: workerDisplayName(record.id, record.name).replace(/[^A-Za-z0-9]/g, "").slice(0, 2).toUpperCase() || "W",
    hue: (index * 47 + 12) % 360,
    task: record.brief,
    summary: record.error ?? "",
    error: record.error ?? null,
    status,
    model: record.model_id ?? record.model ?? "",
    modelSpec: record.model,
    tier: record.tier ?? undefined,
    reasoningEffort: record.reasoning_effort ?? undefined,
    branch: record.branch,
    worktree: record.worktree,
    files: [],
    tokens: record.total_tokens,
    cost: record.cost_micros == null ? null : record.cost_micros / 1_000_000,
    elapsed: elapsedFromMs(record.elapsed_ms),
    turns: record.turns,
    compacting: record.compacting === true,
    progress: progress[status],
    lastActivity: timeFromMs(record.last_activity_ms),
    createdAtMs: record.created_at_ms,
    activity,
    needsYou:
      waitsOnUser(record.pending) || ["needs_you", "conflict", "failed"].includes(record.status),
    fixerFindingId: record.fixer_finding_id ?? undefined,
    kind: workerKindFrom(record.kind),
    reviewRisk: reviewRiskFrom(record.review_risk),
    auditTarget: record.audit_target ?? undefined,
    auditTip: record.audit_tip ?? undefined,
    reportDelivered: record.report_delivered === true,
    routing: record.routing ?? null,
    planId: record.plan_id ?? null,
    planStep: record.plan_step ?? null,
    checks: record.checks.map((check) => ({
      name: check.name,
      status:
        check.status === "passed" || check.status === "failed"
          ? (check.status as Check["status"])
          : "running",
      detail: check.detail ?? "",
      tip: check.tip,
    })),
    findings: record.findings.map(findingFromWire),
    review: record.review
      ? {
          summary: record.review.summary,
          tip: record.review.tip,
          reviewedAtMs: record.review.reviewed_at_ms,
          evidence: reviewEvidenceFrom(record.review.evidence),
        }
      : undefined,
    messages: workerThreadFromRecord(record, messages),
    inbox: (record.inbox ?? []).map(inboxFromWire),
    screenshots: workerScreenshotPaths(record.transcript),
    timeline: workerTimeline(record),
    diff: record.merge
      ? {
          files: record.merge.files,
          additions: record.merge.additions,
          deletions: record.merge.deletions,
        }
      : { files: 0, additions: 0, deletions: 0 },
    merge: record.merge ? mergeFromRecord(record, record.merge) : undefined,
  };
}

function mergeTeamActivity(
  entries: LeadEntry[],
  messages: TeamMessage[],
  workers: TeamWorkerRecord[],
): LeadEntry[] {
  const next = entries.slice();
  // Each entry keeps the name it was recorded with, so it still names its
  // worker after that worker leaves the team list.
  const names = new Map(workers.map((worker) => [worker.id, workerDisplayName(worker.id, worker.name)]));
  const nameOf = (workerId: string, id: string) => {
    const known = names.get(workerId);
    if (known) return known;
    const previous = next.find((entry) => entry.id === id);
    return previous && "workerName" in previous && previous.workerName ? previous.workerName : workerId;
  };
  // A card already shown keeps its place; a new one goes where it happened,
  // never just to the bottom (that replayed every review after a restart).
  const fresh: ActivityCard[] = [];
  const upsert = (entry: ActivityCard) => {
    const index = next.findIndex((candidate) => candidate.id === entry.id);
    if (index === -1) fresh.push(entry);
    else next[index] = entry;
  };
  for (const message of messages) {
    const id = `team-${message.worker_id}-${message.at_ms}`;
    const attributed = next.some((entry) =>
      entry.id !== id && entry.kind === "team-message" &&
      entry.workerId === message.worker_id && message.text.trim().length > 0 &&
      entry.body.includes(message.text),
    );
    if (attributed) {
      const duplicate = next.findIndex((entry) => entry.id === id);
      if (duplicate !== -1) next.splice(duplicate, 1);
      continue;
    }
    upsert({
      id,
      kind: "team-message",
      workerId: message.worker_id,
      workerName: nameOf(message.worker_id, id),
      body: message.text,
      time: timeFromMs(message.at_ms),
      atMs: message.at_ms,
    });
  }
  for (const worker of workers) {
    if (worker.review) {
      const name = workerDisplayName(worker.id, worker.name);
      upsert({
        id: `review-${worker.id}-${worker.review.reviewed_at_ms}`,
        kind: "review",
        workerId: worker.id,
        workerName: name,
        title: `Reviewed ${name}’s work`,
        body: worker.review.summary,
        time: timeFromMs(worker.review.reviewed_at_ms),
        atMs: worker.review.reviewed_at_ms,
      });
    }
    if (worker.merge) {
      const record = mergeFromRecord(worker, worker.merge);
      upsert({
        id: record.id,
        kind: "merge",
        workerId: worker.id,
        workerName: workerDisplayName(worker.id, worker.name),
        record,
        time: record.time,
        atMs: worker.merge.merged_at_ms,
      });
    }
  }
  fresh
    .sort((left, right) => (left.atMs ?? 0) - (right.atMs ?? 0))
    .forEach((card) => insertActivityCard(next, card));
  return next;
}

/** Each worker's streamed activity since its transcript was last saved. */
const workerLive = new Map<string, WorkerLive>();
/** Each worker's saved thread and the record it came from. */
const workerSaved = new Map<string, { generation: number; length: number; thread: Worker["messages"] }>();

/** A worker's thread: its saved conversation plus the live tail on top. */
function composeThread(workerId: string): Worker["messages"] | null {
  const saved = workerSaved.get(workerId);
  if (!saved) return null;
  const live = workerLive.get(workerId);
  if (!live || live.generation !== saved.generation || live.baseLength !== saved.length) {
    workerLive.delete(workerId);
    return saved.thread;
  }
  return [...saved.thread, ...live.entries];
}

/** Each worker's live activity: what its model is doing right now, for every running worker — not just open threads. */
const workerActivity = new Map<string, WorkerActivity>();

/** Fold one streamed worker event into its worker's live activity, for every worker in state. */
function streamWorkerActivity(event: TeamEvent): void {
  const workerId = event.worker_id;
  const current = workerActivity.get(workerId) ?? null;
  const next = applyWorkerEvent(current, event, Date.now());
  if (next === current) return;
  if (next) workerActivity.set(workerId, next);
  else workerActivity.delete(workerId);
  workerStream.push({ token: teamToken, workerId, thread: false });
}

/**
 * A worker whose activity or open thread changed in the maps above. The store
 * write is coalesced (see streamCoalescer.ts): a batch re-reads the maps, so
 * it carries the newest state and can never undo a later team sync.
 */
interface WorkerStreamMark {
  token: number;
  workerId: string;
  thread: boolean;
}

const workerStream = createStreamCoalescer<WorkerStreamMark>(applyWorkerStreamMarks);

function applyWorkerStreamMarks(marks: WorkerStreamMark[]): void {
  const threads = new Set<string>();
  const activities = new Set<string>();
  for (const mark of marks) {
    if (mark.token !== teamToken) continue;
    (mark.thread ? threads : activities).add(mark.workerId);
  }
  const composed = new Map<string, Worker["messages"]>();
  for (const workerId of threads) {
    const thread = composeThread(workerId);
    if (thread) composed.set(workerId, thread);
  }
  if (activities.size === 0 && composed.size === 0) return;
  app.update((state) => ({
    ...state,
    workers: state.workers.map((worker) => {
      const activity = activities.has(worker.id);
      const thread = composed.get(worker.id);
      if (!activity && !thread) return worker;
      return {
        ...worker,
        ...(activity ? { activity: workerActivity.get(worker.id) ?? null } : {}),
        ...(thread ? { messages: thread } : {}),
      };
    }),
  }));
}

/** Stream one worker event into that worker's live activity and open thread. */
function streamWorkerEvent(event: TeamEvent): void {
  streamWorkerActivity(event);
  const saved = workerSaved.get(event.worker_id);
  if (!saved || event.generation < saved.generation) return;
  let live = workerLive.get(event.worker_id);
  if (!live || live.generation !== event.generation || live.baseLength !== saved.length) {
    live = emptyWorkerLive(event.generation, saved.length);
  }
  const next = applyWorkerLiveEvent(live, event);
  if (next === live && workerLive.get(event.worker_id) === live) return;
  workerLive.set(event.worker_id, next);
  if (event.generation !== saved.generation) return;
  workerStream.push({ token: teamToken, workerId: event.worker_id, thread: true });
}

/** Refetch the plan board; failures keep the last board (it is only a view). */
async function syncBoard(): Promise<void> {
  const transport = boardTransport;
  if (!transport) return;
  const token = boardToken;
  try {
    const snapshot = await transport.snapshot();
    if (token !== boardToken) return;
    app.update((state) => ({ ...state, board: boardFromSnapshot(snapshot) }));
  } catch {
    // The board is derived state; the next change event refetches it.
  }
}

function scheduleBoardSync(): void {
  if (boardSyncTimer || !boardTransport) return;
  boardSyncTimer = setTimeout(() => {
    boardSyncTimer = null;
    void syncBoard();
  }, 60);
}

async function disconnectBoard(): Promise<void> {
  boardToken += 1;
  if (boardSyncTimer) {
    clearTimeout(boardSyncTimer);
    boardSyncTimer = null;
  }
  const unlisten = boardUnlisten;
  boardUnlisten = null;
  unlisten?.();
  boardTransport = null;
}

async function connectBoard(): Promise<void> {
  const transport = createBoardTransport();
  if (!transport) return;
  if (!(await probeMnemo2Host())) return;
  await disconnectBoard();
  const token = boardToken;
  boardTransport = transport;
  try {
    const unlisten = await transport.onChanged(() => {
      if (token === boardToken) scheduleBoardSync();
    });
    if (token !== boardToken) {
      unlisten();
      return;
    }
    boardUnlisten = unlisten;
  } catch {
    // Without the event the board still loads on open and on team changes.
  }
  await syncBoard();
}

function applyIndexing(token: number, snapshot: IndexingSnapshot): void {
  if (token !== indexingToken) return;
  app.update((state) => ({ ...state, indexing: newerIndexing(state.indexing, snapshot) }));
}

/** Fetch the indexing status; a failure keeps the last one (events follow). */
async function syncIndexing(): Promise<void> {
  const transport = indexingTransport;
  if (!transport) return;
  const token = indexingToken;
  try {
    applyIndexing(token, await transport.snapshot());
  } catch {
    // No runtime yet (the lead failed to open): nothing is indexing.
  }
}

function disconnectIndexing(): void {
  indexingToken += 1;
  const unlisten = indexingUnlisten;
  indexingUnlisten = null;
  unlisten?.();
  indexingTransport = null;
}

async function connectIndexing(): Promise<void> {
  const transport = createIndexingTransport();
  if (!transport) return;
  if (!(await probeMnemo2Host())) return;
  disconnectIndexing();
  const token = indexingToken;
  indexingTransport = transport;
  try {
    const unlisten = await transport.onChanged((snapshot) => applyIndexing(token, snapshot));
    if (token !== indexingToken) {
      unlisten();
      return;
    }
    indexingUnlisten = unlisten;
  } catch {
    // Without the event the status still loads once the lead opens.
  }
}

async function syncTeam(): Promise<void> {
  const transport = teamTransport;
  if (!transport) return;
  const token = teamToken;
  const projectId = get(app).projectId;
  try {
    const [snapshot, messages] = await Promise.all([
      transport.snapshot(),
      transport.messages(),
    ]);
    if (token !== teamToken) return;
    // Merges tick steps and workers move through statuses; the board follows.
    const before = new Map(get(app).workers.map((worker) => [worker.id, worker]));
    const boardStale =
      snapshot.workers.length !== before.size ||
      snapshot.workers.some((record) => {
        const previous = before.get(record.id);
        return !previous ||
          previous.status !== record.status ||
          (previous.planId ?? null) !== (record.plan_id ?? null) ||
          (previous.planStep ?? null) !== (record.plan_step ?? null);
      });
    if (boardStale) scheduleBoardSync();
    app.update((state) => {
      // A snapshot belongs to the project that requested it: a slow response
      // must never repaint (or reprice) a project the human has since opened.
      if (state.projectId !== projectId) return state;
      const diffWorkerId = state.diffWorkerId ?? state.selectedWorkerId;
      // A worker that left the snapshot must not keep a live activity record.
      for (const id of workerActivity.keys()) {
        if (!snapshot.workers.some((record) => record.id === id)) workerActivity.delete(id);
      }
      return {
        ...state,
        workers: snapshot.workers.map((record, index) => {
          const next = workerFromRecord(record, index, messages);
          workerSaved.set(record.id, {
            generation: record.generation,
            length: record.transcript.messages.length,
            thread: next.messages,
          });
          next.messages = composeThread(record.id) ?? next.messages;
          const previous = state.workers.find((worker) => worker.id === record.id);
          return record.merge ? next : {
            ...next,
            files: previous?.files ?? [],
            diff: previous?.diff ?? next.diff,
          };
        }),
        leadEntries: mergeTeamActivity(state.leadEntries, messages, snapshot.workers),
        teamSlots: snapshot.slots,
        teamActive: snapshot.active,
        projectUsage: projectUsageFrom(snapshot.usage),
        selectedWorkerId: snapshot.workers.some((worker) => worker.id === state.selectedWorkerId)
          ? state.selectedWorkerId
          : snapshot.workers[0]?.id ?? null,
        diffWorkerId: snapshot.workers.some((worker) => worker.id === diffWorkerId)
          ? diffWorkerId
          : snapshot.workers[0]?.id ?? null,
        workerPanelId: snapshot.workers.some((worker) => worker.id === state.workerPanelId)
          ? state.workerPanelId
          : null,
      };
    });
    scheduleDiffSync();
    // A worker that just finished, started waiting for the human, or failed
    // rings the matching sound; a burst across the team collapses to one via
    // the module's debounce window. A snapshot for an already-closed project
    // stays silent (the update above skipped it).
    if (get(app).projectId === projectId) {
      for (const record of snapshot.workers) {
        const sound = workerTransitionSound(before.get(record.id)?.status, record.status);
        if (sound) notifySound(sound);
      }
    }
  } catch (error) {
    if (token === teamToken) noteTeamError(error);
  }
}

function scheduleTeamSync(): void {
  if (teamSyncTimer) return;
  teamSyncTimer = setTimeout(() => {
    teamSyncTimer = null;
    void syncTeam();
  }, 120);
}

async function disconnectTeam(): Promise<void> {
  teamToken += 1;
  workerLive.clear();
  workerSaved.clear();
  workerActivity.clear();
  if (teamSyncTimer) {
    clearTimeout(teamSyncTimer);
    teamSyncTimer = null;
  }
  const unlisten = teamUnlisten;
  teamUnlisten = null;
  unlisten?.();
  teamTransport = null;
}

async function connectTeam(): Promise<void> {
  const transport = createTeamTransport();
  if (!transport) return;
  // Same foreign-host guard as the project path: only the real Mnemo2 host
  // answers the handshake.
  if (!(await probeMnemo2Host())) return;
  await disconnectTeam();
  const token = teamToken;
  teamTransport = transport;
  try {
    const unlisten = await transport.onEvent((event) => {
      if (token !== teamToken) return;
      streamWorkerEvent(event);
      // Streamed content changes no record; only lifecycle events resync.
      if (!STREAM_ONLY_EVENTS.has(String(event.event?.event?.kind ?? ""))) scheduleTeamSync();
    });
    if (token !== teamToken) {
      unlisten();
      return;
    }
    teamUnlisten = unlisten;
    await syncTeam();
  } catch (error) {
    if (token === teamToken) noteTeamError(error);
  }
}

function setWorkerSlots(workerSlots: number): void {
  if (!Number.isInteger(workerSlots) || workerSlots < 1 || workerSlots > 32) {
    app.update((state) => withToast(state, "Worker slots must be a whole number from 1 to 32.", "error"));
    return;
  }
  const transport = requireTeam();
  if (!transport) return;
  const projectId = get(app).projectId;
  void transport.setSlots(workerSlots).then((snapshot) => {
    app.update((state) => {
      if (state.projectId !== projectId) return state;
      return {
        ...state,
        teamSlots: snapshot.slots,
        teamActive: snapshot.active,
        projectUsage: projectUsageFrom(snapshot.usage),
        projects: state.projects.map((project) => project.id === state.projectId
          ? { ...project, workerSlots: snapshot.slots }
          : project),
      };
    });
  }).catch(noteTeamError);
}

function dismissToast() {
  app.update((state) => ({ ...state, toast: null }));
}

function resetDemo() {
  preferenceSave = null;
  if (preferenceTimer) {
    clearTimeout(preferenceTimer);
    preferenceTimer = null;
  }
  leadSessionToken += 1;
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
  steerWorker,
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
