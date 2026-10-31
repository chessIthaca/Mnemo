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