import type { TeamRemoteCheckout } from "./teamState";
import type { Worker, WorkerHost, WorkerStatus } from "./types";

/**
 * The worker statuses a lost host still hurts: the worker is running a turn,
 * about to start one, or waiting on an answer in the middle of one. It mirrors
 * the host's own `is_active` (`src-tauri/src/mnemo2/team.rs:1994`) plus
 * `interrupted`, the resumable worker whose host loss still matters (plan
 * e8cb5112, decision D6). Every other status — finished, merged, stopped,
 * failed — keeps its meaning while its host is away, so the UI shows the host
 * name without an away state.
 */
export const ACTIVE_HOST_STATUSES: readonly WorkerStatus[] = [
  "dispatching",
  "working",
  "needs_you",
  "fixing",
  "interrupted",
];

/**
 * The `Worker.host` of a record's `remote`: which computer runs the worker,
 * since when its link is away and over which SSH connection. Null for a worker
 * on this computer (the snapshot leaves `remote` out entirely).
 */
export function hostFromRemote(remote?: TeamRemoteCheckout | null): WorkerHost | null {
  if (!remote) return null;
  return {
    name: remote.node_name,
    os: remote.os,
    offlineSinceMs: remote.offline_since_ms ?? null,
    ssh: remote.ssh ?? null,
  };
}

/**
 * How long the worker's host has been away, in whole seconds, or null when the
 * host is connected or the worker is one a lost host no longer changes. The
 * caller ticks the clock; `lib/elapsed.ts` already renders the same span.
 */
export function hostAway(worker: Pick<Worker, "host" | "status">, nowMs: number): number | null {
  const offlineSinceMs = worker.host?.offlineSinceMs;
  if (offlineSinceMs == null || !ACTIVE_HOST_STATUSES.includes(worker.status)) return null;
  return Math.max(0, Math.floor((nowMs - offlineSinceMs) / 1000));
}

/**
 * The hover text of the host badge and the detail's host pill: which computer
 * runs the worker, and — for a Claude Code agent placed over SSH — the saved
 * connection its session runs on.
 */
export function hostBadgeTitle(worker: Pick<Worker, "host">): string {
  const host = worker.host;
  if (!host) return "";
  const base = `Runs on ${host.name} (${hostOsLabel(host.os)})`;
  return host.ssh ? `${base}, Claude Code session over SSH ${host.ssh}` : base;
}

/** The OS names the node reports as people write them; anything else unchanged. */
function hostOsLabel(os: string): string {
  const labels: Record<string, string> = { windows: "Windows", macos: "macOS", linux: "Linux" };
  return labels[os] ?? os;
}
