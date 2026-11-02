import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";

/** The channel carrying each new indexing snapshot from the native host. */
export const INDEXING_EVENT_CHANNEL = "mnemo2://indexing";

/** One startup pass: the code index or the memory reconcile. */
export interface IndexPass {
  state: "running" | "done" | "failed";
  /** Units processed so far (files for code, sources for memory). */
  done: number;
  /** Units the pass will process; 0 while it is still counting. */
  total: number;
  /** The summary on success, the error on failure. */
  detail: string | null;
}

/** The project's startup indexing status. */
export interface IndexingSnapshot {
  /** Bumped on every change; a lower revision is stale. */
  revision: number;
  /** The project had no code index when it opened: the UI holds until the first pass ends. */
  firstRun: boolean;
  code: IndexPass | null;
  memory: IndexPass | null;
}

export interface IndexingTransportBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

export interface IndexingTransport {
  snapshot(): Promise<IndexingSnapshot>;
  onChanged(handler: (snapshot: IndexingSnapshot) => void): Promise<UnlistenFn>;
}

export function createTauriIndexingTransport(bindings: IndexingTransportBindings): IndexingTransport {
  return {
    snapshot: () => bindings.invoke<IndexingSnapshot>("indexing_snapshot"),
    onChanged: (handler) =>
      bindings.listen<IndexingSnapshot>(INDEXING_EVENT_CHANNEL, (event) => handler(event.payload)),
  };
}

/** The native indexing transport, or `null` in a browser-only session. */
export function createIndexingTransport(bindings?: IndexingTransportBindings): IndexingTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriIndexingTransport(bindings ?? { invoke, listen });
}

/** Keep the newer of two snapshots. */
export function newerIndexing(
  current: IndexingSnapshot | null,
  next: IndexingSnapshot,
): IndexingSnapshot {
  return current && current.revision > next.revision ? current : next;
}

/** True while a first run's code index has not ended: the project is held. */
export function indexingHoldsProject(snapshot: IndexingSnapshot | null): boolean {
  return snapshot?.firstRun === true && snapshot.code?.state === "running";
}

/** The passes still running, code first. */
export function runningPasses(snapshot: IndexingSnapshot | null): { kind: "code" | "memory"; pass: IndexPass }[] {
  const passes: { kind: "code" | "memory"; pass: IndexPass }[] = [];
  if (snapshot?.code?.state === "running") passes.push({ kind: "code", pass: snapshot.code });
  if (snapshot?.memory?.state === "running") passes.push({ kind: "memory", pass: snapshot.memory });
  return passes;
}
