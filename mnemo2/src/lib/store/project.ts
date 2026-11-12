import { get } from "svelte/store";
import {
  createExternalLeadTransport,
  noExternalLead,
  sameProjectPath,
} from "../externalLeadEngine";
import { initialLeadRuntime } from "../leadState";
import { createProjectTransport, type ProjectRecord } from "../projectEngine";
import { probeMnemo2Host } from "../runtime";
import { emptyProjectUsage } from "../teamState";
import type { Project } from "../types";
import { app, leadError, withToast } from "./core";
import { flushWorkbenchPreferences, hasPendingPreferences } from "./preferences";
import { connectBoard, disconnectBoard } from "./board";
import { connectIndexing, disconnectIndexing, syncIndexing } from "./indexing";
import { connectTeam, disconnectTeam, getTeamTransport, syncTeam } from "./team";
import { disconnectWorkbench, restoreWorkbench } from "./workbench";
import { openTeam } from "./navigation";
import { bumpLeadSession, connectNativeLead, stopNativeLead } from "./lead";
import {
  beginExternalLeadStart,
  externalOpenFailure,
  finishExternalLeadStart,
  handleExternalLeadEvent,
  isCurrentExternalLeadToken,
  pendingExternalLeadStart,
  reportExternalOpenFailure,
  retainExternalLeadUnlisten,
  syncExternalLead,
} from "./externalLead";

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

export async function openProject(projectId: string, allowConflict = false): Promise<void> {
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

export async function loadProjects(): Promise<void> {
  const transport = createProjectTransport();
  if (!transport) return;
  // Only the real Mnemo2 host answers the handshake; another Tauri WebView
  // (the in-app Browser pane, another app's window) never registered these
  // commands.
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
export function initExternalLead(): Promise<void> {
  const pending = pendingExternalLeadStart();
  if (pending) return pending;
  const transport = createExternalLeadTransport();
  if (!transport) return Promise.resolve();
  const token = beginExternalLeadStart();
  const start = (async () => {
    try {
      if (!(await probeMnemo2Host())) return;
      // Listen before loading the registry so no early request is missed.
      const unlisten = await Promise.all([
        transport.onOpenRequest((request) => {
          if (isCurrentExternalLeadToken(token)) void openExternalRoot(request.root);
        }),
        transport.onChanged((event) => handleExternalLeadEvent(token, event)),
      ]);
      if (!retainExternalLeadUnlisten(token, unlisten)) return;
      await loadProjects();
      const root = await transport.startupProject();
      if (root && isCurrentExternalLeadToken(token)) await openExternalRoot(root);
    } catch {
      // A host without the external-lead commands has nothing to start.
    }
  })();
  finishExternalLeadStart(start);
  return start;
}

export async function openFolder(): Promise<void> {
  const transport = createProjectTransport();
  if (!transport) {
    app.update((state) => withToast(state, "Opening a folder needs the native shell.", "error"));
    return;
  }
  // A Tauri WebView is not necessarily the Mnemo2 host: the in-app Browser
  // pane (or another app's window) is one too, and its IPC never registered
  // this command (Tauri answers "not allowed by ACL"). Confirm the handshake
  // before invoking anything.
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

export async function closeProject(): Promise<void> {
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

export function cancelCloseProject(): void {
  app.update((state) => ({ ...state, closeProjectPending: false }));
}

export async function confirmCloseProject(): Promise<void> {
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

export async function reviewProject(projectId: string): Promise<void> {
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

export function cancelProjectConflict(): void {
  app.update((state) => ({ ...state, projectConflict: null }));
}

export async function confirmProjectConflict(): Promise<void> {
  const conflict = get(app).projectConflict;
  if (conflict) await openProject(conflict.projectId, true);
}

export function requestCleanup(projectId: string): void {
  app.update((state) => ({ ...state, cleanupProjectId: projectId }));
}

export function cancelCleanup(): void {
  if (!get(app).cleanupBusy) app.update((state) => ({ ...state, cleanupProjectId: null }));
}

export async function confirmCleanup(): Promise<void> {
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

export async function openNewProject(): Promise<void> {
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

export async function createProject(name: string): Promise<void> {
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

export function cancelNewProject(): void {
  if (!get(app).creatingProject) app.update((state) => ({ ...state, newProjectParent: null }));
}
