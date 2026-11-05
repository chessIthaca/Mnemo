import type { Worker } from "./types";

/** What a worker's reply box does right now: steer it, message it, or nothing. */
export type WorkerBoxMode = "steer" | "message" | "blocked" | "done";

/** Everything a worker's reply box needs to render itself. */
export interface WorkerBox {
  mode: WorkerBoxMode;
  /** Whether the box accepts a message at all. */
  canSend: boolean;
  /** The textarea's placeholder: what the box does, or why it can't. */
  placeholder: string;
  /** The send button's accessible name. */
  sendLabel: string;
  /** Whether the box offers Pause — the steerable interrupt, not Stop. */
  canPause: boolean;
}

/**
 * Decide what a worker's reply box offers.
 *
 * Done means the task is over and no live session is left to steer: a merged,
 * merging, completed, in-review or cancelled worker, or an audit that delivered
 * its report. The finished-audit rule runs before the status rule so a completed
 * audit still says it finished its audit rather than just "is done". A stopped or
 * interrupted worker has no session either (Stop removes it) and points at
 * Resume; a compacting one is refused by the backend, so it is blocked while it
 * compacts. A later fix round moves a worker to `fixing`, which reopens the box as
 * a steer — and because this closes only the box, the lead can still send a
 * completed worker back to work through `team_message`.
 *
 * The backend already picks the command: `TeamRuntime::message` steers a busy
 * worker and prompts an idle one. The box only says which one this is.
 */
export function workerBox(
  worker: Pick<Worker, "name" | "status" | "kind" | "reportDelivered" | "compacting">,
): WorkerBox {
  const n = worker.name;
  const label = `Send message to ${n}`;
  const done = { mode: "done" as const, canSend: false, sendLabel: label, canPause: false };
  if (worker.kind === "audit" && (worker.reportDelivered === true || worker.status === "completed")) {
    return { ...done, placeholder: `${n} finished its audit` };
  }
  if (
    worker.status === "merged" ||
    worker.status === "merging" ||
    worker.status === "cancelled" ||
    worker.status === "completed" ||
    worker.status === "in_review"
  ) {
    return { ...done, placeholder: `${n} is done` };
  }
  if (worker.status === "stopped" || worker.status === "interrupted") {
    return { mode: "blocked", canSend: false, placeholder: `Resume ${n} to message it`, sendLabel: label, canPause: false };
  }
  if (worker.compacting) {
    return { mode: "blocked", canSend: false, placeholder: `${n} is compacting`, sendLabel: label, canPause: false };
  }
  if (
    worker.status === "working" ||
    worker.status === "dispatching" ||
    worker.status === "fixing"
  ) {
    return {
      mode: "steer",
      canSend: true,
      placeholder: `Steer ${n}…`,
      sendLabel: `Steer ${n}`,
      canPause: worker.status === "working" || worker.status === "dispatching",
    };
  }
  return { mode: "message", canSend: true, placeholder: `Message ${n}`, sendLabel: label, canPause: false };
}
