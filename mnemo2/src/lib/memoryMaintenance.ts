/**
 * The Memory tab's upkeep: clean up memory, rebuild search, rebuild index.
 *
 * The three commands live in `src-tauri/src/mnemo2/memory_maintenance.rs` and
 * start a background run each; the run's `started` / `progress` / `done` /
 * `failed` events arrive on {@link MAINTENANCE_CHANNEL}, so the tab shows a
 * live progress line and a one-line result instead of waiting on the call.
 *
 * The event → UI-state fold ({@link applyMaintenanceEvent}) is pure, so the
 * contract is unit-testable without a host. One transport, one channel: the
 * tab's read-only data layer (`memoryView.ts`) stays untouched.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";

/** The Tauri event channel an upkeep run streams its progress on. */
export const MAINTENANCE_CHANNEL = "memory://maintenance";

/** Which upkeep action an event belongs to. */
export type MaintenanceOp = "cleanup" | "rebuild" | "index";

/** One event on {@link MAINTENANCE_CHANNEL} — the Rust `MaintenanceEvent`. */
export type MaintenanceEvent =
  | { type: "started"; op: MaintenanceOp }
  | { type: "progress"; op: MaintenanceOp; done: number; total: number; phase: string }
  | { type: "done"; op: MaintenanceOp; summary: string }
  | { type: "failed"; op: MaintenanceOp; error: string };

/** The UI state for one upkeep action (progress line + result line). */
export interface OpState {
  /** Whether the action is running (progress line visible). */
  running: boolean;
  /** Completed units from the latest progress tick. */
  done: number;
  /** Total units from the latest tick (`0` = a phase with no units). */
  total: number;
  /** The latest stage label ("consolidating" / "embedding" / …). */
  phase: string;
  /** The terminal success summary (`null` until the action completes). */
  summary: string | null;
  /** The terminal failure text (`null` unless the action failed). */
  error: string | null;
}

/** The idle state every action starts (and restarts) in. */
export function idleOp(): OpState {
  return { running: false, done: 0, total: 0, phase: "", summary: null, error: null };
}

/**
 * Fold one event into an action's UI state: a start begins afresh, a tick
 * moves the line, and either terminal event replaces the result.
 */
export function applyMaintenanceEvent(state: OpState, event: MaintenanceEvent): OpState {
  switch (event.type) {
    case "started":
      // A fresh start clears any previous result and stale progress.
      return { ...idleOp(), running: true };
    case "progress":
      return {
        ...state,
        running: true,
        done: event.done,
        total: event.total,
        phase: event.phase,
        summary: null,
        error: null,
      };
    case "done":
      return { ...state, running: false, phase: "", summary: event.summary, error: null };
    case "failed":
      return { ...state, running: false, phase: "", summary: null, error: event.error };
  }
}

/**
 * The progress line's text: "embedding 50/200" while the stage counts units,
 * the bare stage label when it has none, "working" before the first tick, and
 * nothing at all when the action is not running.
 */
export function progressLabel(state: OpState): string {
  if (!state.running) return "";
  if (state.total > 0) return `${state.phase} ${state.done}/${state.total}`;
  return state.phase || "working";
}

/** One upkeep action as the tab renders it. */
export interface MaintenanceAction {
  /** The action's event/command token. */
  op: MaintenanceOp;
  /** The row's heading. */
  title: string;
  /** What the action does, in one or two plain sentences. */
  description: string;
  /** The button's text. */
  button: string;
}

/** The three actions, in the order the tab shows them. */
export const MAINTENANCE_OPS: ReadonlyArray<MaintenanceAction> = [
  {
    op: "cleanup",
    title: "Clean up memory",
    description:
      "Turns finished sessions' raw tool events into summaries, deletes the raw rows and compacts the database. Running sessions are left alone.",
    button: "Clean up",
  },
  {
    op: "rebuild",
    title: "Rebuild search",
    description:
      "Re-embeds every memory with the current embedding model, rebuilds the text index and compacts the database. Use it when search results look stale or wrong.",
    button: "Rebuild",
  },
  {
    op: "index",
    title: "Rebuild index",
    description:
      "Re-reads plans, reviews, knowledge records and the backlog in .coding into the index. Records you or the agents wrote are never changed.",
    button: "Rebuild",
  },
];

/** The upkeep I/O the Memory tab needs. */
export interface MaintenanceTransport {
  /** Start one action; it answers once the run is accepted, not when it ends. */
  run(op: MaintenanceOp): Promise<void>;
  /** Subscribe to the run's progress events. */
  onEvent(handler: (event: MaintenanceEvent) => void): Promise<UnlistenFn>;
}

export interface MaintenanceTransportBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

export function createTauriMaintenanceTransport(
  bindings: MaintenanceTransportBindings,
): MaintenanceTransport {
  return {
    // Literal command names: the host's registry guard reads them from source.
    run: (op) =>
      op === "cleanup"
        ? bindings.invoke<void>("memory_cleanup")
        : op === "rebuild"
          ? bindings.invoke<void>("memory_rebuild_search")
          : bindings.invoke<void>("memory_rebuild_index"),
    onEvent: (handler) =>
      bindings.listen<MaintenanceEvent>(MAINTENANCE_CHANNEL, (event) => handler(event.payload)),
  };
}

/**
 * The upkeep transport, or `null` outside the desktop app (a plain browser has
 * no host to run anything).
 */
export function createMaintenanceTransport(
  bindings?: MaintenanceTransportBindings,
): MaintenanceTransport | null {
  if (bindings) return createTauriMaintenanceTransport(bindings);
  if (isTauriRuntime()) return createTauriMaintenanceTransport({ invoke, listen });
  return null;
}
