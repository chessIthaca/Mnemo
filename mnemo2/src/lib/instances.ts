import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

/**
 * The machine-wide instances overview: every Mnemo2 window running on this
 * machine, its open project, its workers and today's cost (plan d3336a8a).
 *
 * The backend joins the per-instance run files with each window's
 * `POST /instance/summary`; the bearer tokens never reach the page, so the
 * page names a pid and the backend does the calling.
 */

/** How often the list refreshes while Mission Control is open. */
export const INSTANCES_REFRESH_MS = 10_000;

/** One running Mnemo2 (mirrors the backend's `InstanceRow`). */
export interface InstanceRow {
  /** The instance's process id. */
  pid: number;
  /** The open project root; null on the start screen. */
  project: string | null;
  /** The instance's version. */
  version: string;
  /** When the instance started, in Unix epoch milliseconds. */
  started_ms: number;
  /** Whether this is the window asking. */
  is_self: boolean;
  /** Whether the instance answered in time; false shows "not answering". */
  answered: boolean;
  /** Workers with active work in progress. */
  workers_running: number;
  /** Workers blocked on the person. */
  workers_waiting: number;
  /** Today's spend in micro-USD (the Cost tab's per-day estimate). */
  cost_today_micros: number;
}

/** What the instances panel needs from the backend; injectable for tests. */
export interface InstancesTransport {
  /** Every running instance, this window included. */
  list(): Promise<InstanceRow[]>;
  /** Ask another running instance to bring its window to the front. */
  show(pid: number): Promise<void>;
}

/** The real transport: Tauri commands that talk over the run files. */
export const instances: InstancesTransport = {
  async list(): Promise<InstanceRow[]> {
    if (!isTauriRuntime()) return [];
    return invoke<InstanceRow[]>("instances_list");
  },
  async show(pid: number): Promise<void> {
    if (!isTauriRuntime()) return;
    await invoke<void>("instance_show", { pid });
  },
};
