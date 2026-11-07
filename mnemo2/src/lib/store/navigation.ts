import type { RepoPathRef } from "../repoPaths";
import type { TraceEntry } from "../trace";
import type { TeamFilter } from "../types";
import { app } from "./core";
import { openWorkbench } from "./workbench";

export function openLead() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "lead" : state.route,
    teamPeekOpen: false,
  }));
}

export function openTeam() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "team" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
  }));
}

/** Show the project's spend and savings; needs an open project. */
export function openCost() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "cost" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
  }));
}

/** Show the project's code graph; needs an open project. */
export function openGraph() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "graph" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
  }));
}

/** Show every board plan with its expandable details; needs an open project. */
export function openPlans() {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "plans" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
  }));
}

/** Show the project's knowledge records and memories; needs an open project. */
export function openMemory() {
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
export function openTrace(filters?: TraceEntry) {
  app.update((state) => ({
    ...state,
    route: state.projectId ? "trace" : state.route,
    teamPeekOpen: false,
    workbenchOpen: false,
    traceEntry: filters ?? null,
  }));
}

/** Clear a consumed Trace hand-over so returning to the route starts unfiltered. */
export function clearTraceEntry() {
  app.update((state) => (state.traceEntry === null ? state : { ...state, traceEntry: null }));
}

/**
 * Open a plan document: the Memory tab's link to `.coding/plans/<id>.md`. The
 * Plans route expands the plan, reads its document and scrolls it into view;
 * an id the board does not hold shows a short not-found note.
 */
export function openPlanDocument(planId: string) {
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
export function clearPlanDocument() {
  app.update((state) => (state.planDocumentId === null ? state : { ...state, planDocumentId: null }));
}

/**
 * Open a project file in the Workbench: the Memory tab's link to a path a
 * record names. The Files tab reads it through its own `openPath` and reveals
 * `line` when the reference carries one.
 */
export function openFileInWorkbench(path: string, line: number | null = null) {
  const target = path.trim();
  if (target === "") return;
  const at = line !== null && Number.isInteger(line) && line > 0 ? line : null;
  openWorkbench("files");
  app.update((state) => ({ ...state, workbenchFile: { path: target, line: at } }));
}

/** Clear a consumed file hand-over so returning to the tab opens the tree. */
export function clearWorkbenchFile() {
  app.update((state) => (state.workbenchFile === null ? state : { ...state, workbenchFile: null }));
}

/**
 * Open a repository path a Markdown view names: a plan document goes to the
 * Plans route, anything else to the Workbench's Files tab at its line. This is
 * the one action every Markdown surface passes as `onOpenPath`.
 */
export function openRepoPath(ref: RepoPathRef) {
  if (ref.kind === "plan" && ref.planId !== null) openPlanDocument(ref.planId);
  else openFileInWorkbench(ref.path, ref.line);
}

export function toggleTeamPeek() {
  app.update((state) => ({ ...state, teamPeekOpen: !state.teamPeekOpen }));
}

export function selectWorker(workerId: string) {
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

export function setTeamFilter(teamFilter: TeamFilter) {
  app.update((state) => ({ ...state, teamFilter }));
}

/**
 * Stream a worker's output beside the lead chat. The route stays on "lead",
 * the Workbench gives way, and unknown ids are ignored.
 */
export function openWorkerPanel(workerId: string) {
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
export function closeWorkerPanel() {
  app.update((state) => ({ ...state, workerPanelId: null }));
}
