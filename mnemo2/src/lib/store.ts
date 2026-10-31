import { derived, get, writable } from "svelte/store";
import { createLeadTransport, type LeadApprovalDecision, type LeadTransport } from "./leadEngine";
import { createProjectTransport, type ProjectPreferences, type ProjectRecord, type ProjectTransport } from "./projectEngine";
import { probeMnemo2Host } from "./runtime";
import { createTeamTransport, type TeamTransport } from "./teamEngine";
import { createWorkbenchTransport, type WorkbenchTransport } from "./workbenchEngine";
import { createBrowserTransport, type BrowserTransport } from "./browserEngine";
import { workerScreenshotPaths } from "./workbenchState";
import {
  emptyProjectUsage,
  findingFromWire,
  findingToWire,
  projectUsageFrom,
  reviewEvidenceFrom,
  reviewEvidenceToWire,
  reviewRiskFrom,
  workerKindFrom,
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
  messageText,
} from "./leadState";
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
  Worker,
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
let workbenchTransport: WorkbenchTransport | null = null;
let diffRequest = 0;
let diffSyncTimer: ReturnType<typeof setTimeout> | null = null;
let browserTransport: BrowserTransport | null = null;
let browserUnlisten: (() => void)[] = [];
let browserToken = 0;
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

function handleNativeLeadEvent(token: number, event: LeadEvent): void {
  if (token !== leadSessionToken) return;
  app.update((state) => applyLeadEvent(state, event));
  if (leadEventAffectsUsage(event)) {
    // The durable per-project accumulator changed; re-read the team snapshot
    // (debounced and project-guarded) so the header cost follows the lead too.
    scheduleTeamSync();
  }
  if (leadEventNeedsSnapshot(event)) {
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
    browserUrl: "about:blank",
    browserTitle: "",
    browserLoading: false,
    browserSupported: null,
    browserError: null,
    browserImage: null,
    plan: null,
    planItems: [],
    diff: null,
    diffFiles: [],
    diffWorkerId: null,
    diffLoading: false,
    diffError: null,
    projectConflict: null,
    cleanupProjectId: null,
    cleanupBusy: false,
    closeProjectPending: false,
    newProjectParent: null,
    creatingProject: false,
    toast: null,
  };
}

export const app = writable<AppState>(createInitialState());

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

function withToast(state: AppState, message: string): AppState {
  return { ...state, toast: { id: ++sequence, message } };
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
      ? withToast(state, `Could not save workbench preferences: ${leadError(error)}`)
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
      workbench_width: state.workbenchWidth,
      browser_url: state.browserUrl,
      diff_worker_id: state.diffWorkerId,
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
      diff: null,
      diffFiles: [],
      diffError: null,
    } : state);
    if (preferences.workbench_open && preferences.workbench_tab === "diff") {
      void refreshDiff();
    }
  } catch (error) {
    app.update((state) => state.projectId === project.id
      ? withToast(state, `Could not restore workbench preferences: ${leadError(error)}`)
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
    app.update((state) => withToast(state, "That project is not registered."));
    return;
  }
  if (preferenceSave) await flushWorkbenchPreferences();
  if (project.available === false) {
    app.update((state) => withToast(state, project.description || "The project folder is unavailable."));
    return;
  }
  const transport = createProjectTransport();
  if (transport) {
    try {
      if (!(await probeMnemo2Host())) {
        app.update((state) => withToast(state, "The native project host is unavailable."));
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
      app.update((state) => withToast(state, leadError(error)));
      return;
    }
  }
  leadSessionToken += 1;
  await stopNativeLead();
  await disconnectTeam();
  await disconnectWorkbench();
  app.update((state) => ({
    ...state,
    route: "lead",
    projectId,
    projectConflict: null,
    cleanupProjectId: null,
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
    browserUrl: "about:blank",
    browserTitle: "",
    browserLoading: false,
    browserSupported: null,
    browserError: null,
    browserImage: null,
    toast: null,
  }));
  // Dependent startup IPC (the team snapshot and event subscription) is
  // rejected by the native host with "Mnemo2 has no open lead; call lead_open
  // first" until the automatic lead open has completed, so the lead opens
  // first — ordering only: no timers, no retries, and the backend guard stays
  // for real misuse. A genuine lead-open failure is already surfaced on the
  // lead runtime by connectNativeLead.
  await connectNativeLead(project, allowConflict);
  // A project switch during the lead open owns the rest of the startup.
  if (get(app).projectId !== projectId) return;
  await connectTeam();
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
    app.update((state) => withToast(state, `Could not load projects: ${leadError(error)}`));
  }
}

async function openFolder(): Promise<void> {
  const transport = createProjectTransport();
  if (!transport) {
    app.update((state) => withToast(state, "Opening a folder needs the native shell."));
    return;
  }
  // A Tauri WebView is not necessarily the Mnemo2 host: Mnemo1's Browser tab
  // has one too, and its IPC never registered this command (Tauri answers
  // "not allowed by ACL"). Confirm the handshake before invoking anything.
  if (!(await probeMnemo2Host())) {
    app.update((state) => withToast(state, "Opening a folder needs the native shell."));
    return;
  }
  try {
    const folder = await transport.pickFolder();
    if (!folder) return;
    const record = await transport.add(folder);
    await loadProjects();
    await openProject(record.path);
  } catch (error) {
    app.update((state) => withToast(state, `Could not open the folder: ${leadError(error)}`));
  }
}

async function closeProjectNow(): Promise<void> {
  if (preferenceSave) await flushWorkbenchPreferences();
  leadSessionToken += 1;
  await stopNativeLead();
  await disconnectTeam();
  await disconnectWorkbench();
  app.update((state) => ({
    ...state,
    route: "start",
    projectId: null,
    projectConflict: null,
    cleanupProjectId: null,
    cleanupBusy: false,
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
      ...withToast(state, `Could not stop the team: ${leadError(error)}`),
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
      ...withToast(state, `Cleanup failed: ${leadError(error)}`),
      cleanupBusy: false,
    }));
  }
}

async function openNewProject(): Promise<void> {
  const transport = createProjectTransport();
  if (!transport) {
    app.update((state) => withToast(state, "Project creation needs the native shell."));
    return;
  }
  try {
    if (!(await probeMnemo2Host())) {
      app.update((state) => withToast(state, "Project creation needs the native shell."));
      return;
    }
    const parent = await transport.pickFolder();
    if (parent) app.update((state) => ({ ...state, newProjectParent: parent }));
  } catch (error) {
    app.update((state) => withToast(state, leadError(error)));
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
    app.update((state) => withToast(state, `Could not create the project: ${leadError(error)}`));
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
  app.update((state) => ({ ...state, workbenchOpen: true, workbenchTab }));
  if (workbenchTab === "diff" && get(app).leadRuntime.mode === "native") void refreshDiff();
  schedulePreferenceSave();
}

function setWorkbenchWidth(workbenchWidth: number) {
  app.update((state) => ({
    ...state,
    workbenchWidth: Math.min(720, Math.max(320, workbenchWidth)),
  }));
  schedulePreferenceSave();
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
  return browserCommand((transport) => transport.setVisible(visible));
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
        return withToast({ ...state, leadEntries: entries, leadRuntime: runtime }, failure);
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
      app.update((current) => withToast(current, "The native lead is not connected."));
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
function showToast(message: string): void {
  app.update((state) => withToast(state, message));
}

async function interruptLead(): Promise<void> {
  const transport = leadTransport;
  if (!transport) {
    const error = new Error("The native lead is not connected, so it cannot be interrupted.");
    app.update((state) => withToast(state, error.message));
    throw error;
  }
  const token = leadSessionToken;
  try {
    await transport.interrupt();
  } catch (error) {
    if (token === leadSessionToken) {
      app.update((state) => withToast(state, leadError(error)));
    }
    throw error;
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
        app.update((state) => withToast(state, leadError(error)));
      }
    });
}

function answerLead(questionId: string, answer: LeadAnswerInput) {
  const transport = leadTransport;
  if (!transport) return;
  const token = leadSessionToken;
  void transport
    .answer(questionId, answer)
    .then(() => refreshNativeLead(token))
    .catch((error) => {
      if (token === leadSessionToken) {
        app.update((state) => withToast(state, leadError(error)));
      }
    });
}

function setLeadModel(spec: string) {
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
    .setModel(spec)
    .then((snapshot) => {
      if (token === leadSessionToken) {
        app.update((current) => applyLeadSnapshot(current, snapshot));
      }
    })
    .catch((error) => {
      if (token === leadSessionToken) {
        app.update((current) => withToast(current, leadError(error)));
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
    app.update((state) => withToast(state, "The native team runtime is not connected."));
    return null;
  }
  return teamTransport;
}

function noteTeamError(error: unknown): void {
  app.update((state) => withToast(state, leadError(error)));
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

async function dispatchWorker(brief: string, model?: string | null): Promise<void> {
  const transport = requireTeam();
  if (!transport) return;
  const task = brief.trim();
  if (!task) return;
  try {
    await transport.dispatch(task, model ?? null);
    await syncTeam();
  } catch (error) {
    noteTeamError(error);
  }
}

function stopWorker(workerId: string) {
  const transport = requireTeam();
  if (!transport) return;
  void transport
    .stop(workerId)
    .then(() => syncTeam())
    .catch(noteTeamError);
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
    app.update((state) => withToast(state, `Unknown finding '${findingId}'.`));
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

function workerMessages(record: TeamWorkerRecord, messages: TeamMessage[]): Worker["messages"] {
  const transcript: Worker["messages"] = [];
  record.transcript.messages.forEach((message, index) => {
    if (message.role !== "user" && message.role !== "assistant") return;
    const body = messageText(message.content).trim();
    if (!body) return;
    transcript.push({
      id: `worker-${record.id}-${record.generation}-${index}`,
      from: message.role === "user" ? "lead" : "worker",
      body,
      time: "restored",
    });
  });
  const inbox = messages
    .filter((message) => message.worker_id === record.id)
    .map((message) => ({
      id: `team-${message.worker_id}-${message.at_ms}`,
      from: "worker" as const,
      body: message.text,
      time: timeFromMs(message.at_ms),
    }));
  return [...transcript, ...inbox];
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
      label: record.merge.revert_commit ? "Merge reverted" : `Merged into ${record.merge.target}`,
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
  } else if (["failed", "conflict"].includes(record.status)) {
    timeline.push({
      id: `${record.id}-attention`,
      label: "Needs attention",
      detail: record.error ?? "The worker needs attention.",
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
    name: record.name,
    initials: record.name.replace(/[^A-Za-z0-9]/g, "").slice(0, 2).toUpperCase() || "W",
    hue: (index * 47 + 12) % 360,
    task: record.brief,
    summary: record.error ?? "",
    status,
    model: record.model_id ?? record.model ?? "",
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
    needsYou:
      record.pending != null || ["needs_you", "conflict", "failed"].includes(record.status),
    fixerFindingId: record.fixer_finding_id ?? undefined,
    kind: workerKindFrom(record.kind),
    reviewRisk: reviewRiskFrom(record.review_risk),
    auditTarget: record.audit_target ?? undefined,
    auditTip: record.audit_tip ?? undefined,
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
    messages: workerMessages(record, messages),
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
  const upsert = (entry: LeadEntry) => {
    const index = next.findIndex((candidate) => candidate.id === entry.id);
    if (index === -1) next.push(entry);
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
      body: message.text,
      time: timeFromMs(message.at_ms),
    });
  }
  for (const worker of workers) {
    if (worker.review) {
      upsert({
        id: `review-${worker.id}-${worker.review.reviewed_at_ms}`,
        kind: "review",
        workerId: worker.id,
        title: "Lead review recorded",
        body: worker.review.summary,
        time: timeFromMs(worker.review.reviewed_at_ms),
      });
    }
    if (worker.merge) {
      const record = mergeFromRecord(worker, worker.merge);
      upsert({
        id: record.id,
        kind: "merge",
        workerId: worker.id,
        record,
        time: record.time,
      });
    }
  }
  return next;
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
    app.update((state) => {
      // A snapshot belongs to the project that requested it: a slow response
      // must never repaint (or reprice) a project the human has since opened.
      if (state.projectId !== projectId) return state;
      const diffWorkerId = state.diffWorkerId ?? state.selectedWorkerId;
      return {
        ...state,
        workers: snapshot.workers.map((record, index) => {
          const next = workerFromRecord(record, index, messages);
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
        selectedWorkerId: state.selectedWorkerId ?? snapshot.workers[0]?.id ?? null,
        diffWorkerId: snapshot.workers.some((worker) => worker.id === diffWorkerId)
          ? diffWorkerId
          : snapshot.workers[0]?.id ?? null,
      };
    });
    scheduleDiffSync();
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
    const unlisten = await transport.onEvent(() => {
      if (token === teamToken) scheduleTeamSync();
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
    app.update((state) => withToast(state, "Worker slots must be a whole number from 1 to 32."));
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
  void disconnectWorkbench();
  app.set(createInitialState());
}

export const actions = {
  openProject,
  loadProjects,
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
  toggleTeamPeek,
  selectWorker,
  setTeamFilter,
  openWorkbench,
  closeWorkbench,
  setWorkbenchTab,
  setWorkbenchWidth,
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
  approveLead,
  answerLead,
  setLeadModel,
  retryLead,
  dispatchWorker,
  replyWorker,
  steerWorker,
  stopWorker,
  pauseWorker,
  resumeWorker,
  compactWorker,
  syncTeam,
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
};
