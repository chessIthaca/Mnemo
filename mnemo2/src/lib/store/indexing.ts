import { createIndexingTransport, newerIndexing, type IndexingSnapshot, type IndexingTransport } from "../indexingEngine";
import { probeMnemo2Host } from "../runtime";
import { app } from "./core";

let indexingTransport: IndexingTransport | null = null;
let indexingUnlisten: (() => void) | null = null;
let indexingToken = 0;

function applyIndexing(token: number, snapshot: IndexingSnapshot): void {
  if (token !== indexingToken) return;
  app.update((state) => ({ ...state, indexing: newerIndexing(state.indexing, snapshot) }));
}

/** Fetch the indexing status; a failure keeps the last one (events follow). */
export async function syncIndexing(): Promise<void> {
  const transport = indexingTransport;
  if (!transport) return;
  const token = indexingToken;
  try {
    applyIndexing(token, await transport.snapshot());
  } catch {
    // No runtime yet (the lead failed to open): nothing is indexing.
  }
}

export function disconnectIndexing(): void {
  indexingToken += 1;
  const unlisten = indexingUnlisten;
  indexingUnlisten = null;
  unlisten?.();
  indexingTransport = null;
}

export async function connectIndexing(): Promise<void> {
  const transport = createIndexingTransport();
  if (!transport) return;
  if (!(await probeMnemo2Host())) return;
  disconnectIndexing();
  const token = indexingToken;
  indexingTransport = transport;
  try {
    const unlisten = await transport.onChanged((snapshot) => applyIndexing(token, snapshot));
    if (token !== indexingToken) {
      unlisten();
      return;
    }
    indexingUnlisten = unlisten;
  } catch {
    // Without the event the status still loads once the lead opens.
  }
}
