import { derived, get, writable } from "svelte/store";
import {
  createDiffFiles,
  createLeadEntries,
  createPlanItems,
  createProjects,
  createWorkers,
} from "./mock";
import { createLeadTransport, type LeadApprovalDecision, type LeadTransport } from "./leadEngine";
import { createProjectTransport, type ProjectRecord } from "./projectEngine";
import {
  applyLeadEvent,
  applyLeadSnapshot,
  initialLeadRuntime,
  leadEventNeedsSnapshot,
} from "./leadState";
import { canMerge } from "./review";
import type {
  AppState,
  LeadAnswerInput,
  LeadEvent,
  MergeRecord,
  Project,
  TeamFilter,
  Worker,
  WorkbenchTab,
} from "./types";

let sequence = 0;
const nextId = (prefix: string) => `${prefix}-${++sequence}`;

let leadTransport: LeadTransport | null = null;
let leadUnlisten: (() => void) | null = null;
let leadSessionToken = 0;

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
  const workers = createWorkers();
  return {
    route: "start",
    projectId: null,
    projects: createProjects(),
    workers,
    leadEntries: createLeadEntries(workers),
    leadRuntime: initialLeadRuntime(),
    selectedWorkerId: "ana",
    teamPeekOpen: false,
    teamFilter: "all",
    workbenchOpen: false,
    workbenchTab: "browser",
    workbenchWidth: 480,
    browserUrl: "http://localhost:5180/settings",
    planItems: createPlanItems(),
    diffFiles: createDiffFiles(),
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
  app.update((state) => ({
    ...state,
    route: "lead",
    projectId,
    leadRuntime: initialLeadRuntime(),
    teamPeekOpen: false,
    workbenchOpen: false,
    toast: null,
  }));
  void connectNativeLead(project);
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
  app.update((state) => ({
    ...state,
    route: "start",
    projectId: null,
    leadRuntime: initialLeadRuntime(),
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
  app.update((current) => ({
    ...current,
    leadEntries: [
      ...current.leadEntries,
      {
        id: nextId("user"),
        kind: "message",
        author: "user",
        body: message,
        time: "now",
      },
      {
        id: nextId("lead"),
        kind: "message",
        author: "lead",
        body: "Understood — I’ll keep that in the **plan of record**. For this clickable phase, open **Team → Ana** to resolve the two findings and produce a new merge record.",
        time: "now",
      },
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

function replyWorker(workerId: string, body: string) {
  const message = body.trim();
  if (!message) return;
  app.update((state) =>
    updateWorker(state, workerId, (worker) => ({
      ...worker,
      status: worker.status === "failed" || worker.status === "conflict" ? "working" : worker.status,
      needsYou: false,
      lastActivity: "now",
      messages: [
        ...worker.messages,
        { id: nextId("lead-reply"), from: "lead", body: message, time: "now" },
        {
          id: nextId("worker-ack"),
          from: "worker",
          body: "Got it — I’ll fold that into the current branch and report back with checks.",
          time: "now",
        },
      ],
    })),
  );
}

function steerWorker(workerId: string) {
  replyWorker(
    workerId,
    "Steer: keep the current scope, avoid unrelated edits, and surface any conflict before continuing.",
  );
  app.update((state) => withToast(state, "Steering note added to the worker thread."));
}

function stopWorker(workerId: string) {
  app.update((state) => {
    const worker = state.workers.find((item) => item.id === workerId);
    if (!worker) return state;
    if (worker.status === "merged") return withToast(state, "A merged worker cannot be stopped.");
    return withToast(
      updateWorker(state, workerId, (item) => ({
        ...item,
        status: "cancelled",
        needsYou: false,
        fixerFindingId: undefined,
        lastActivity: "now",
      })),
      `${worker.name} stopped. No branch changes were made in this prototype.`,
    );
  });
}

function fixFinding(workerId: string, findingId: string) {
  app.update((state) => {
    const worker = state.workers.find((item) => item.id === workerId);
    const finding = worker?.findings.find((item) => item.id === findingId);
    if (!worker || !finding || finding.status !== "open") return state;

    return withToast(
      updateWorker(state, workerId, (item) => ({
        ...item,
        status: "in_review",
        checks: [{ name: "cargo test --workspace", status: "passed", detail: "review_fix tip · 38s" }],
        findings: item.findings.map((entry) =>
          entry.id === findingId
            ? {
                ...entry,
                status: "fixed",
                fixCommit: `review-${findingId}`,
                evidence: "Lead review_fix · affected checks re-run",
              }
            : entry,
        ),
      })),
      "Tiny review fix applied and the merge-time checks passed again.",
    );
  });
}

function spawnFixer(workerId: string, findingId: string) {
  app.update((state) => {
    const worker = state.workers.find((item) => item.id === workerId);
    const finding = worker?.findings.find((item) => item.id === findingId);
    if (!worker || !finding || finding.status !== "open") return state;

    return withToast(
      updateWorker(state, workerId, (item) => ({
        ...item,
        status: "fixing",
        needsYou: false,
        fixerFindingId: findingId,
        lastActivity: "now",
        checks: [{ name: "cargo test --workspace", status: "running", detail: "Fixer branch tip" }],
        timeline: [
          ...item.timeline.map((step) =>
            step.status === "active" ? { ...step, status: "done" as const } : step,
          ),
          {
            id: nextId("fixer-step"),
            label: "Fixer applying review delta",
            detail: finding.title,
            status: "active",
          },
        ],
        messages: [
          ...item.messages,
          {
            id: nextId("fixer-message"),
            from: "lead",
            body: `Fixer brief: ${finding.title}. Stay on ${item.branch}; re-run the affected checks.`,
            time: "now",
          },
        ],
      })),
      `Fixer dispatched on ${worker.branch}.`,
    );
  });
}

function completeFixer(workerId: string, findingId: string) {
  app.update((state) => {
    const worker = state.workers.find((item) => item.id === workerId);
    const finding = worker?.findings.find((item) => item.id === findingId);
    if (!worker || !finding || finding.status !== "open" || worker.status !== "fixing" || worker.fixerFindingId !== findingId) {
      return state;
    }

    const next = updateWorker(state, workerId, (item) => ({
      ...item,
      status: "in_review",
      needsYou: true,
      fixerFindingId: undefined,
      lastActivity: "now",
      checks: [{ name: "cargo test --workspace", status: "passed", detail: "Fixer tip · 41s" }],
      findings: item.findings.map((entry) =>
        entry.id === findingId
          ? {
              ...entry,
              status: "fixed",
              fixCommit: `fixer-${findingId}`,
              evidence: "Fixer delta reviewed · affected checks re-run",
            }
          : entry,
      ),
      timeline: [
        ...item.timeline.map((step) =>
          step.status === "active" ? { ...step, status: "done" as const } : step,
        ),
        {
          id: nextId("review-step"),
          label: "Lead re-review",
          detail: "Delta verified; ready for merge decision",
          status: "active",
        },
      ],
    }));

    return withToast(
      {
        ...next,
        leadEntries: [
          ...state.leadEntries,
          {
            id: nextId("review"),
            kind: "review",
            workerId,
            title: `Fixer delta verified · ${worker.name}`,
            body: finding.title,
            time: "now",
          },
        ],
      },
      "Fixer commit verified. The worker is ready for the lead merge decision.",
    );
  });
}

function wontfixFinding(workerId: string, findingId: string, justification: string) {
  const reason = justification.trim();
  app.update((state) => {
    if (!reason) return withToast(state, "A written justification is required for wontfix.");
    const worker = state.workers.find((item) => item.id === workerId);
    const finding = worker?.findings.find((item) => item.id === findingId);
    if (!worker || !finding || finding.status !== "open") return state;

    return withToast(
      updateWorker(state, workerId, (item) => ({
        ...item,
        status: "in_review",
        findings: item.findings.map((entry) =>
          entry.id === findingId ? { ...entry, status: "wontfix", justification: reason } : entry,
        ),
      })),
      "Finding marked wontfix with a written justification.",
    );
  });
}

function reviewSummary(worker: Worker): string {
  if (worker.findings.length === 0) return "No findings";
  const fixed = worker.findings.filter((finding) => finding.status === "fixed").length;
  const wontfix = worker.findings.filter((finding) => finding.status === "wontfix").length;
  return `${worker.findings.length} findings · ${fixed} fixed · ${wontfix} wontfix`;
}

function mergeWorker(workerId: string) {
  app.update((state) => {
    const worker = state.workers.find((item) => item.id === workerId);
    if (!worker) return state;
    const eligibility = canMerge(worker);
    if (!eligibility.ok) return withToast(state, eligibility.reason);

    const project = state.projects.find((item) => item.id === state.projectId);
    const record: MergeRecord = {
      id: nextId("merge"),
      workerId,
      commit: `demo-${workerId}-merge`,
      branch: worker.branch,
      target: project?.branch ?? "wt/mnemo",
      files: worker.diff.files,
      additions: worker.diff.additions,
      deletions: worker.diff.deletions,
      checks: worker.checks.map((check) => check.name),
      reviewSummary: reviewSummary(worker),
      model: worker.model,
      cost: worker.cost,
      time: "now",
    };

    const next = updateWorker(state, workerId, (item) => ({
      ...item,
      status: "merged",
      needsYou: false,
      progress: 100,
      lastActivity: "now",
      merge: record,
      timeline: [
        ...item.timeline.map((step) =>
          step.status === "active" ? { ...step, status: "done" as const } : step,
        ),
        {
          id: nextId("merge-step"),
          label: "Merged by TeamManager",
          detail: `${item.branch} → ${record.target}`,
          status: "done",
          time: "now",
        },
      ],
    }));

    return withToast(
      {
        ...next,
        leadEntries: [
          ...state.leadEntries,
          { id: nextId("entry-merge"), kind: "merge", workerId, record, time: "now" },
        ],
      },
      `Merged into ${record.target}. The record is now in the lead chat.`,
    );
  });
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
  replyWorker,
  steerWorker,
  stopWorker,
  fixFinding,
  spawnFixer,
  completeFixer,
  wontfixFinding,
  mergeWorker,
  setWorkerSlots,
  dismissToast,
  resetDemo,
};
