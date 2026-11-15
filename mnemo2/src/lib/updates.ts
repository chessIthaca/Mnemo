import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";

/**
 * The auto-update feature's frontend half.
 *
 * The desktop host owns the whole update lifecycle: it checks GitHub 30 s
 * after start and then every 6 h, downloads a found release in the
 * background, and installs it only when the user restarts from the update
 * banner. This module is the thin client that talks to the host — three
 * Tauri commands and one event channel — plus the two presentation helpers
 * the About page and the banner share. A build without the host's updater
 * reports the `disabled` phase, so the same UI serves every build.
 */

/** The channel the host emits every update-state change on. */
export const UPDATE_EVENT_CHANNEL = "mnemo2://update-state";

/**
 * What the host reports about its update cycle (the Rust `UpdateState`,
 * tagged and snake_case on the wire):
 *
 * - `disabled` — this build never updates itself. A source build reports
 *   the reason "source_build"; the browser preview reports "preview".
 * - `idle` — nothing to do; `last_checked_ms` is the last check's time.
 * - `checking` — a check is running.
 * - `downloading` — a newer release is downloading; `total` is null while
 *   the size is still unknown.
 * - `ready` — a release is downloaded and waits for the user's restart.
 * - `failed` — the last check failed; the host tries again later.
 */
export type UpdateState =
  | { phase: "disabled"; reason: string }
  | { phase: "idle"; last_checked_ms: number | null }
  | { phase: "checking" }
  | { phase: "downloading"; version: string; downloaded: number; total: number | null }
  | { phase: "ready"; version: string; notes: string | null }
  | { phase: "failed"; message: string; last_checked_ms: number | null };

/**
 * The answer to `update_restart`: either the orderly shutdown-and-install
 * started, or workers are still running and the host wants the user to
 * confirm interrupting them.
 */
export type UpdateRestart =
  | { result: "started" }
  | { result: "workers_running"; count: number };

/** What the update client needs from the Tauri bridge (injectable for tests). */
export interface UpdateBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

/** The update UI's host commands (see `createUpdatesClient`). */
export interface UpdatesClient {
  /** The current update state, without touching the network. */
  state(): Promise<UpdateState>;
  /** Ask for a check right now; resolves to the state the check produced. */
  check(): Promise<UpdateState>;
  /**
   * Install the downloaded update through an orderly restart; `force`
   * overrides the running-worker guard. Rejects when nothing is ready.
   */
  restart(force: boolean): Promise<UpdateRestart>;
  /**
   * Call `handler` on every state the host emits; the promise resolves to
   * the unsubscribe function.
   */
  onState(handler: (state: UpdateState) => void): Promise<UnlistenFn>;
}

/**
 * The update client over the given bindings. Inside the desktop app every
 * call reaches the host commands; in a plain browser there is no host, so
 * `state`/`check` answer the preview stand-in, `restart` refuses, and
 * `onState` resolves a no-op unsubscribe.
 */
export function createUpdatesClient(
  bindings: UpdateBindings = { invoke, listen },
): UpdatesClient {
  return {
    state: async () => {
      if (!isTauriRuntime()) return { phase: "disabled", reason: "preview" };
      return bindings.invoke<UpdateState>("update_state");
    },
    check: async () => {
      if (!isTauriRuntime()) return { phase: "disabled", reason: "preview" };
      return bindings.invoke<UpdateState>("update_check");
    },
    restart: async (force: boolean) => {
      if (!isTauriRuntime()) throw new Error("Updates need the desktop app.");
      return bindings.invoke<UpdateRestart>("update_restart", { force });
    },
    onState: async (handler) => {
      if (!isTauriRuntime()) return () => {};
      return bindings.listen<UpdateState>(UPDATE_EVENT_CHANNEL, (event) =>
        handler(event.payload),
      );
    },
  };
}

/** The app's update client; tests plant their own with `createUpdatesClient`. */
export const updates = createUpdatesClient();

/**
 * How long ago a timestamp was, in the coarse buckets an update line needs:
 * "just now", then minutes, hours, and days.
 */
export function ago(ms: number): string {
  if (ms < 60_000) return "just now";
  if (ms < 3_600_000) return `${Math.floor(ms / 60_000)} min ago`;
  if (ms < 86_400_000) return `${Math.floor(ms / 3_600_000)} h ago`;
  return `${Math.floor(ms / 86_400_000)} d ago`;
}

/**
 * The one line the About page shows for a state. `nowMs` is the clock the
 * "checked …" age is measured against.
 */
export function describeUpdateState(state: UpdateState, nowMs: number): string {
  switch (state.phase) {
    case "disabled":
      if (state.reason === "source_build") {
        return "Automatic updates are off in this build (built from source).";
      }
      if (state.reason === "preview") return "Updates are checked by the desktop app.";
      return `Automatic updates are off: ${state.reason}.`;
    case "idle":
      return state.last_checked_ms === null
        ? "Mnemo2 checks for updates automatically."
        : `Up to date · checked ${ago(nowMs - state.last_checked_ms)}`;
    case "checking":
      return "Checking for updates…";
    case "downloading":
      return state.total === null
        ? `Downloading Mnemo2 ${state.version}…`
        : `Downloading Mnemo2 ${state.version}… ${Math.floor((state.downloaded * 100) / state.total)}%`;
    case "ready":
      return `Mnemo2 ${state.version} is ready. Restart to update.`;
    case "failed":
      return `Could not check for updates: ${state.message}. Mnemo2 tries again later.`;
  }
}

/** Whether the user may ask for a check right now (idle, or after a failure). */
export function canCheck(state: UpdateState): boolean {
  return state.phase === "idle" || state.phase === "failed";
}
