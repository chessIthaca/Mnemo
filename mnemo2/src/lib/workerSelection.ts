import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { sameProjectPath } from "./externalLeadEngine";
import { isTauriRuntime } from "./runtime";
import type { Worker } from "./types";

/**
 * Workers the lead waits on: dispatched, working or in a fixer run.
 * TeamManager wakes the lead when one finishes.
 */
export function selectRunningWorkers(workers: Worker[]): Worker[] {
  return workers.filter((worker) => ["dispatching", "working", "fixing"].includes(worker.status));
}

/** Workers waiting on a human, busy lead or not. Same predicate as needsYouCount. */
export function selectNeedsYouWorkers(workers: Worker[]): Worker[] {
  return workers.filter((worker) => worker.needsYou);
}

/** The channel asking the frontend to show a worker in mission control. */
export const OPEN_WORKER_CHANNEL = "mnemo2://open-worker";

/** Claude Code asking Mnemo2 to show one of a project's workers. */
export interface OpenWorkerRequest {
  /** The registered project whose worker Claude Code is pointing at. */
  root: string;
  /** The worker to show in mission control. */
  worker_id: string;
}

/** The listener bindings a native open-worker transport is created with. */
export interface OpenWorkerTransportBindings {
  listen: typeof listen;
}

/** How the frontend hears `mnemo2://open-worker` events from the host. */
export interface OpenWorkerTransport {
  onOpenWorker(handler: (request: OpenWorkerRequest) => void): Promise<UnlistenFn>;
}

/** The transport the Mnemo2 host serves; tests pass their own bindings. */
export function createTauriOpenWorkerTransport(
  bindings: OpenWorkerTransportBindings,
): OpenWorkerTransport {
  return {
    onOpenWorker: (handler) =>
      bindings.listen<OpenWorkerRequest>(OPEN_WORKER_CHANNEL, (event) => handler(event.payload)),
  };
}

/** The native open-worker transport, or `null` in a browser-only session. */
export function createOpenWorkerTransport(
  bindings?: OpenWorkerTransportBindings,
): OpenWorkerTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriOpenWorkerTransport(bindings ?? { listen });
}

/** A show-worker request the app cannot honor as asked. */
export type OpenWorkerMiss =
  | { kind: "missing"; workerId: string; projectName: string }
  | { kind: "foreign"; root: string }
  | { kind: "closed" };

/** What a show-worker request means for the project this window shows. */
export type OpenWorkerOutcome = { kind: "select"; workerId: string } | OpenWorkerMiss;

/**
 * Decide what a show-worker request means: it is honored only when its root is
 * the open project, so another root names a project the window is not showing,
 * and a worker id the team does not know cannot be selected.
 */
export function openWorkerOutcome(
  request: OpenWorkerRequest,
  workers: Pick<Worker, "id">[],
  project: { path: string; name: string } | null,
): OpenWorkerOutcome {
  if (!project) return { kind: "closed" };
  if (!sameProjectPath(request.root, project.path)) return { kind: "foreign", root: request.root };
  const known = workers.some((worker) => worker.id === request.worker_id);
  return known
    ? { kind: "select", workerId: request.worker_id }
    : { kind: "missing", workerId: request.worker_id, projectName: project.name };
}

/** The short notice explaining a show-worker request that could not be honored. */
export function openWorkerNotice(miss: OpenWorkerMiss): string {
  switch (miss.kind) {
    case "missing":
      return `Claude Code asked to show worker ${miss.workerId}, which is not in ${miss.projectName}.`;
    case "foreign":
      return `Claude Code asked to show a worker in ${miss.root}, which is not open.`;
    case "closed":
      return "Claude Code asked to show a worker, but no project is open.";
  }
}