import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";

/** The payload-less channel the native host emits after every backlog write. */
export const BACKLOG_EVENT_CHANNEL = "mnemo2://backlog-changed";

/** An item's dispatch status — the store's `BacklogStatus` in snake_case. */
export type BacklogStatus = "pending" | "in_flight" | "done" | "failed" | "cant_resolve";

/**
 * One `.coding/backlog.jsonl` entry — the kernel store's `BacklogItem`.
 *
 * The optional fields are omitted from the wire format while unset (the
 * store's `skip_serializing_if`): a plain pending idea carries only `id`,
 * `text`, `images`, `status` and `created_at`.
 */
export interface BacklogItem {
  id: string;
  text: string;
  /** Relative paths to gitignored sidecar images; empty for most ideas. */
  images: string[];
  status: BacklogStatus;
  created_at: number;
  /** The failure note of a finished item. */
  note?: string | null;
  /** Deferred: kept in the list, dimmed and badged; absent means false. */
  deferred?: boolean;
  /** The dispatched plan's root id, when a plan was dispatched from this item. */
  plan_id?: string | null;
  /** That plan's title at dispatch. */
  plan_title?: string | null;
  /** Soft-delete stamp; absent for every live item. */
  deleted_at?: number | null;
}

export interface BacklogBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

/**
 * The Plans tab's backlog I/O. Every mutating call returns the store's FULL
 * live list in display order, so the view replaces its state instead of
 * patching a local copy.
 */
export interface BacklogTransport {
  items(): Promise<BacklogItem[]>;
  add(text: string): Promise<BacklogItem[]>;
  edit(id: string, text: string): Promise<BacklogItem[]>;
  remove(id: string): Promise<BacklogItem[]>;
  reorder(ids: string[]): Promise<BacklogItem[]>;
  setDeferred(id: string, deferred: boolean): Promise<BacklogItem[]>;
  setDone(id: string, done: boolean): Promise<BacklogItem[]>;
  clearFinished(): Promise<BacklogItem[]>;
  planThis(id: string): Promise<BacklogItem[]>;
  onChanged(handler: () => void): Promise<UnlistenFn>;
}

/** The stand-in transport planted for the preview page (see `seedBacklogPreview`). */
let previewSeed: BacklogTransport | null = null;

/**
 * Dev-only seam for the preview page: plant a stand-in transport so the
 * `plans` scene renders sample backlog rows in a plain browser, where no Tauri
 * host exists. Ignored inside the desktop app — `createBacklogTransport` only
 * consults the seed when there is no Tauri runtime, so production can never
 * serve planted data.
 */
export function seedBacklogPreview(transport: BacklogTransport | null): void {
  previewSeed = transport;
}

export function createTauriBacklogTransport(bindings: BacklogBindings): BacklogTransport {
  return {
    items: () => bindings.invoke<BacklogItem[]>("backlog_items"),
    add: (text) => bindings.invoke<BacklogItem[]>("backlog_add", { text }),
    edit: (id, text) => bindings.invoke<BacklogItem[]>("backlog_edit", { id, text }),
    remove: (id) => bindings.invoke<BacklogItem[]>("backlog_remove", { id }),
    reorder: (ids) => bindings.invoke<BacklogItem[]>("backlog_reorder", { ids }),
    setDeferred: (id, deferred) =>
      bindings.invoke<BacklogItem[]>("backlog_set_deferred", { id, deferred }),
    setDone: (id, done) => bindings.invoke<BacklogItem[]>("backlog_set_done", { id, done }),
    clearFinished: () => bindings.invoke<BacklogItem[]>("backlog_clear_finished"),
    planThis: (id) => bindings.invoke<BacklogItem[]>("plan_this", { id }),
    onChanged: (handler) => bindings.listen(BACKLOG_EVENT_CHANNEL, () => handler()),
  };
}

/** The native backlog transport, or the planted preview seed in a plain browser. */
export function createBacklogTransport(bindings?: BacklogBindings): BacklogTransport | null {
  if (bindings) return createTauriBacklogTransport(bindings);
  if (isTauriRuntime()) return createTauriBacklogTransport({ invoke, listen });
  return previewSeed;
}

/** The queue's pending ideas, in the store's order. */
export function pendingItems(items: BacklogItem[]): BacklogItem[] {
  return items.filter((item) => item.status === "pending");
}

/** Whether the item is deferred (the flag is omitted while false). */
export function isDeferred(item: BacklogItem): boolean {
  return item.deferred === true;
}

/** The ideas the lead is working on right now ("With the lead"), in store order. */
export function inFlightItems(items: BacklogItem[]): BacklogItem[] {
  return items.filter((item) => item.status === "in_flight");
}

/** The finished ideas (done, failed or cant-resolve), in the store's order. */
export function finishedItems(items: BacklogItem[]): BacklogItem[] {
  return items.filter(
    (item) => item.status === "done" || item.status === "failed" || item.status === "cant_resolve",
  );
}

/** How many finished ideas wait behind "Clear finished". */
export function finishedCount(items: BacklogItem[]): number {
  return finishedItems(items).length;
}

/**
 * Move one pending idea `delta` places inside the queue and return the full
 * id order to persist (the pending queue first in its new order, then the
 * rest), or `null` when the move would leave the queue.
 */
export function movePending(items: BacklogItem[], id: string, delta: number): string[] | null {
  const queue = pendingItems(items).map((item) => item.id);
  const from = queue.indexOf(id);
  const to = from + delta;
  if (from < 0 || to < 0 || to >= queue.length) return null;
  const [moved] = queue.splice(from, 1);
  queue.splice(to, 0, moved);
  return [...queue, ...items.filter((item) => item.status !== "pending").map((item) => item.id)];
}

/**
 * The idea's first line, shortened to `max` characters — the row's accessible
 * label, so screen readers announce which idea a button acts on.
 */
export function ideaTitle(item: BacklogItem, max = 60): string {
  const line = item.text.split("\n")[0].trim();
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}
