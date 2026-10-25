import { derived, get, writable } from "svelte/store";
import { createLeadTransport, type LeadApprovalDecision, type LeadTransport } from "./leadEngine";
import { createProjectTransport, type ProjectRecord } from "./projectEngine";
import { createTeamTransport, type TeamTransport } from "./teamEngine";
import type { TeamMessage, TeamWorkerRecord } from "./teamState";
import {
  applyLeadEvent,
  applyLeadSnapshot,
  initialLeadRuntime,
  leadEventNeedsSnapshot,
} from "./leadState";
import type {
  AppState,
  Check,
  LeadAnswerInput,
  LeadEntry,
  LeadEvent,
  Project,
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

function leadError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
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
  if (leadEventNeedsSnapshot(event)) {
    setTimeout(() => {
      if (token === leadSessionToken) void refreshNativeLead(token);
    }, 25);
  }
}

async function connectNativeLead(project: Project): Promise<void> {
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
    const snapshot = await transport.open({ workspaceRoot: project.path });
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
    leadRuntime: initialLeadRuntime(),
    selectedWorkerId: null,
    teamPeekOpen: false,
    teamFilter: "all",
    teamSlots: 0,
    teamActive: 0,
    workbenchOpen: false,
    workbenchTab: "browser",
    workbenchWidth: 480,
    browserUrl: "http://localhost:5180/settings",
    planItems: [],
    diffFiles: [],
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

function projectFromRecord(record: ProjectRecord): Project {
  return {
    id: record.path,
    name: record.name,
    path: record.path,
    branch: "",
    leadModel: "",
    description: "",
    lastSession: "",
    workerSlots: 3,
    budgetSpent: 0,
    budgetLimit: 0,
  };
}

function openProject(projectId: string) {
  const project = get(app).projects.find((item) => item.id === projectId);
  if (!project) {
    app.update((state) => withToast(state, "That project is not registered."));
    return;
  }
  leadSessionToken += 1;
  void stopNativeLead();
  void disconnectTeam();
  app.update((state) => ({
    ...state,
    route: "lead",
    projectId,
    leadRuntime: initialLeadRuntime(),
    workers: [],
    selectedWorkerId: null,
    teamPeekOpen: false,
    workbenchOpen: false,
    toast: null,
  }));
  void connectNativeLead(project);
  void connectTeam();
}

async function loadProjects(): Promise<void> {
  const transport = createProjectTransport();
  if (!transport) return;
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
  try {
    const folder = await transport.pickFolder();
    if (!folder) return;
    const record = await transport.add(folder);
    await loadProjects();
    openProject(record.path);
  } catch (error) {
    app.update((state) => withToast(state, `Could not open the folder: ${leadError(error)}`));
  }
}

function closeProject() {
  leadSessionToken += 1;
  void stopNativeLead();
  void disconnectTeam();
  app.update((state) => ({
    ...state,
    route: "start",
    projectId: null,
    leadRuntime: initialLeadRuntime(),
    workers: [],
    selectedWorkerId: null,
    teamPeekOpen: false,
    workbenchOpen: false,
  }));
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

function openWorkbench(workbenchTab: WorkbenchTab = "browser") {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "lead" : state.route,
    teamPeekOpen: false,
    workbenchOpen: true,
    workbenchTab,
  }));
}

function closeWorkbench() {
  app.update((state) => ({ ...state, workbenchOpen: false }));
}

function setWorkbenchTab(workbenchTab: WorkbenchTab) {
  app.update((state) => ({ ...state, workbenchOpen: true, workbenchTab }));
}

function setWorkbenchWidth(workbenchWidth: number) {
  app.update((state) => ({
    ...state,
    workbenchWidth: Math.min(720, Math.max(320, workbenchWidth)),
  }));
}

function openBrowser(url: string) {
  app.update((state) => ({
    ...state,
    workbenchOpen: true,
    workbenchTab: "browser",
    browserUrl: url,
  }));
}

function navigateBrowser(url: string) {
  const value = url.trim();
  if (!value) return;
  const browserUrl = /^https?:\/\//i.test(value) ? value : `https://${value}`;
  app.update((state) => ({ ...state, browserUrl, workbenchOpen: true }));
}

async function sendNativeLeadMessage(message: string): Promise<void> {
  const transport = leadTransport;
  if (!transport) return;
  const token = leadSessionToken;
  app.update((state) => ({
    ...state,
    leadEntries: [
      ...state.leadEntries,
      {
        id: nextId("user"),
        kind: "message",
        author: "user",
        body: message,
        time: "now",
      },
    ],
    leadRuntime: {
      ...state.leadRuntime,
      busy: true,
      error: null,
      streamingEntryId: null,
    },
  }));
  try {
    await transport.prompt(message);
  } catch (error) {
    if (token === leadSessionToken) {
      app.update((state) => ({
        ...state,
        leadRuntime: {
          ...state.leadRuntime,
          busy: false,
          error: leadError(error),
        },
      }));
    }
  }
}

function sendLeadMessage(body: string) {
  const message = body.trim();
  if (!message) return;
  const state = get(app);
  if (state.leadRuntime.mode === "native") {
    if (!leadTransport) {
      app.update((current) => withToast(current, "The native lead is not connected."));
      return;
    }
    if (state.leadRuntime.status !== "ready") {
      app.update((current) => withToast(current, "The lead is still connecting."));
      return;
    }
    if (state.leadRuntime.busy) return;
    void sendNativeLeadMessage(message);
    return;
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
      { id: nextId("user"), kind: "message", author: "user", body: message, time: "now" },
    ],
  }));
}

function interruptLead() {
  const transport = leadTransport;
  if (!transport) return;
  const token = leadSessionToken;
  void transport.interrupt().catch((error) => {
    if (token === leadSessionToken) {
      app.update((state) => withToast(state, leadError(error)));
    }
  });
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

function phaseThree(action: string) {
  app.update((state) =>
    withToast(state, `${action} lands with the real review/merge runtime in Phase 3.`),
  );
}

function fixFinding(_workerId: string, _findingId: string) {
  phaseThree("Applying a review fix");
}

function spawnFixer(_workerId: string, _findingId: string) {
  phaseThree("Dispatching a fixer");
}

function completeFixer(_workerId: string, _findingId: string) {
  phaseThree("Completing a fixer round");
}

function wontfixFinding(_workerId: string, _findingId: string, _justification: string) {
  phaseThree("Marking a finding wontfix");
}

function mergeWorker(_workerId: string) {
  phaseThree("Merging a worker branch");
}

function workerFromRecord(
  record: TeamWorkerRecord,
  index: number,
  messages: TeamMessage[],
): Worker {
  const status = record.status as WorkerStatus;
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
    tokens: record.used_tokens,
    cost: 0,
    elapsed: "",
    turns: 0,
    progress: status === "completed" ? 100 : 0,
    lastActivity: "now",
    needsYou: record.status === "needs_you" || record.pending != null,
    checks: record.checks.map((check) => ({
      name: check.name,
      status:
        check.status === "passed" || check.status === "failed"
          ? (check.status as Check["status"])
          : "running",
      detail: check.detail ?? "",
    })),
    findings: [],
    messages: messages
      .filter((message) => message.worker_id === record.id)
      .map((message) => ({
        id: `team-${message.worker_id}-${message.at_ms}`,
        from: "worker",
        body: message.text,
        time: "now",
      })),
    timeline: [],
    diff: { files: 0, additions: 0, deletions: 0 },
  };
}

function mergeTeamMessages(
  entries: LeadEntry[],
  messages: TeamMessage[],
  workers: TeamWorkerRecord[],
): LeadEntry[] {
  const known = new Set(entries.map((entry) => entry.id));
  const additions: LeadEntry[] = [];
  for (const message of messages) {
    const id = `team-${message.worker_id}-${message.at_ms}`;
    if (known.has(id)) continue;
    const worker = workers.find((item) => item.id === message.worker_id);
    additions.push({
      id,
      kind: "team-message",
      workerId: message.worker_id,
      body: message.text,
      time: worker?.name ?? new Date(message.at_ms).toLocaleTimeString(),
    });
  }
  return additions.length > 0 ? [...entries, ...additions] : entries;
}

async function syncTeam(): Promise<void> {
  const transport = teamTransport;
  if (!transport) return;
  const token = teamToken;
  try {
    const [snapshot, messages] = await Promise.all([
      transport.snapshot(),
      transport.messages(),
    ]);
    if (token !== teamToken) return;
    app.update((state) => ({
      ...state,
      workers: snapshot.workers.map((record, index) =>
        workerFromRecord(record, index, messages),
      ),
      leadEntries: mergeTeamMessages(state.leadEntries, messages, snapshot.workers),
      teamSlots: snapshot.slots,
      teamActive: snapshot.active,
      selectedWorkerId:
        state.selectedWorkerId ?? snapshot.workers[0]?.id ?? null,
    }));
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

function setWorkerSlots(workerSlots: number) {
  if (!Number.isFinite(workerSlots)) return;
  app.update((state) => ({
    ...state,
    projects: state.projects.map((project) =>
      project.id === state.projectId
        ? { ...project, workerSlots: Math.max(1, Math.floor(workerSlots)) }
        : project,
    ),
  }));
}

function dismissToast() {
  app.update((state) => ({ ...state, toast: null }));
}

function resetDemo() {
  leadSessionToken += 1;
  void stopNativeLead();
  void disconnectTeam();
  app.set(createInitialState());
}

export const actions = {
  openProject,
  loadProjects,
  openFolder,
  closeProject,
  openLead,
  openTeam,
  toggleTeamPeek,
  selectWorker,
  setTeamFilter,
  openWorkbench,
  closeWorkbench,
  setWorkbenchTab,
  setWorkbenchWidth,
  openBrowser,
  navigateBrowser,
  sendLeadMessage,
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
  syncTeam,
  fixFinding,
  spawnFixer,
  completeFixer,
  wontfixFinding,
  mergeWorker,
  setWorkerSlots,
  dismissToast,
  resetDemo,
};
