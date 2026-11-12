import { get } from "svelte/store";
import {
  applyExternalLeadEvent,
  createExternalLeadTransport,
  externalLeadFromStatus,
  type ExternalLeadEvent,
} from "../externalLeadEngine";
import { probeMnemo2Host } from "../runtime";
import {
  createOpenWorkerTransport,
  openWorkerNotice,
  openWorkerOutcome,
  type OpenWorkerRequest,
} from "../workerSelection";
import type { Project } from "../types";
import { app, withToast } from "./core";
import { selectWorker } from "./navigation";

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

/** The open project's external lead as the native host reports it now. */
export async function syncExternalLead(project: Project): Promise<void> {
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

/** Applies one attach/detach event while its start's token is still current. */
export function handleExternalLeadEvent(token: number, event: ExternalLeadEvent): void {
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
export function reportExternalOpenFailure(root: string, reason: string): void {
  const transport = createExternalLeadTransport();
  void transport?.reportOpenFailure(root, reason).catch(() => undefined);
}

/**
 * Why `project` is not open and leading after `openProject`, or null when it
 * is. A failed lead open keeps the project selected and shows the error on
 * the lead runtime, so the project id alone is not success.
 */
export function externalOpenFailure(project: Project): string | null {
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

export function disconnectExternalLead(): void {
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
export async function initOpenWorker(): Promise<void> {
  if (openWorkerStart) return openWorkerStart;
  const transport = createOpenWorkerTransport();
  if (!transport) return Promise.resolve();
  const token = ++openWorkerToken;
  const start = (async () => {
    try {
      // Like the external-lead listeners: a Tauri WebView is not necessarily
      // this host, and only the real Mnemo2 host emits this event.
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

export function disconnectOpenWorker(): void {
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

// `initExternalLead` itself lives in ./project (it opens projects through
// `openExternalRoot` and `loadProjects`, and ./project imports this module,
// never the reverse), so these accessors expose the private external-lead
// start state it needs, one statement each.

/** The external-lead start already running, or null when none is. */
export function pendingExternalLeadStart(): Promise<void> | null {
  return externalLeadStart;
}

/** Takes the next external-lead token for a start attempt. */
export function beginExternalLeadStart(): number {
  externalLeadToken += 1;
  return externalLeadToken;
}

/** Whether `token` still matches the active external-lead token. */
export function isCurrentExternalLeadToken(token: number): boolean {
  return token === externalLeadToken;
}

/** Stores a start's listeners while its token is current; drops stale ones. */
export function retainExternalLeadUnlisten(token: number, unlisten: (() => void)[]): boolean {
  if (token !== externalLeadToken) {
    for (const stop of unlisten) stop();
    return false;
  }
  externalLeadUnlisten = unlisten;
  return true;
}

/** Records the start promise so repeat calls return it instead of restarting. */
export function finishExternalLeadStart(start: Promise<void>): void {
  externalLeadStart = start;
}
