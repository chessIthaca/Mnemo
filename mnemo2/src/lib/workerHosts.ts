/**
 * The Settings → Worker hosts page's data: the paired computers that run
 * Mnemo workers (`worker_hosts_*` commands, the same code the lead's
 * `team_hosts` tool uses) and the small label rules the page shows.
 */

import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

/** How often the page re-reads the host list. */
export const WORKER_HOSTS_REFRESH_MS = 10_000;

/** One paired worker host. */
export interface WorkerHost {
  name: string;
  node_id: string;
  os: string;
  connected: boolean;
  slots: number | null;
  busy: number | null;
  catalog_ready: boolean | null;
  /** "passed", "pending" or "failed: <reason>". */
  checks: string;
  last_seen_ms: number | null;
  ssh: string | null;
  claude_home: boolean | null;
}

export interface HubInfo {
  listening: boolean;
  address: string | null;
}

export interface WorkerHostsList {
  hub: HubInfo;
  hosts: WorkerHost[];
}

/** A pairing code and the two commands to run on the other computer. */
export interface PairOffer {
  code: string;
  expires_in_s: number;
  hub: HubInfo;
  commands: string[];
  note: string;
}

export interface WorkerHostsTransport {
  list(): Promise<WorkerHostsList>;
  pair(name?: string): Promise<PairOffer>;
  revoke(name: string): Promise<unknown>;
  link(name: string, ssh: string): Promise<unknown>;
  unlink(name: string): Promise<unknown>;
}

/** The shell transport; null in a plain browser. */
export function createWorkerHostsTransport(): WorkerHostsTransport | null {
  if (!isTauriRuntime()) return null;
  return {
    list: () => invoke<WorkerHostsList>("worker_hosts_list"),
    pair: (name) => invoke<PairOffer>("worker_hosts_pair", { name }),
    revoke: (name) => invoke<unknown>("worker_hosts_revoke", { name }),
    link: (name, ssh) => invoke<unknown>("worker_hosts_link", { name, ssh }),
    unlink: (name) => invoke<unknown>("worker_hosts_unlink", { name }),
  };
}

function relative(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h} h ago`;
  return `${Math.floor(h / 24)} d ago`;
}

/** "connected", "away since 2 min ago" or "never connected". */
export function hostStateLabel(host: WorkerHost, nowMs: number): string {
  if (host.connected) return "connected";
  if (host.last_seen_ms === null) return "never connected";
  return `away since ${relative(nowMs - host.last_seen_ms)}`;
}

export type ChecksTone = "ok" | "pending" | "bad";

/** The checks state as text and a tone. */
export function checksLabel(checks: string): { text: string; tone: ChecksTone } {
  if (checks === "passed") return { text: "checks passed", tone: "ok" };
  if (checks === "pending") return { text: "checking…", tone: "pending" };
  const reason = checks.startsWith("failed:") ? checks.slice("failed:".length).trim() : checks;
  return { text: `checks failed: ${reason}`, tone: "bad" };
}

/** A friendly OS name. */
export function osLabel(os: string): string {
  if (os === "windows") return "Windows";
  if (os === "macos") return "macOS";
  return os;
}
