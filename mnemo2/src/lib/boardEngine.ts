import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";
import type { BoardSnapshot } from "./boardState";

/** The payload-less channel the native host emits when the board changes. */
export const BOARD_EVENT_CHANNEL = "mnemo2://board-changed";

export interface BoardTransportBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

export interface BoardTransport {
  snapshot(): Promise<BoardSnapshot>;
  /** One plan's markdown — read lazily when a card's details open. */
  document(planId: string): Promise<string>;
  onChanged(handler: () => void): Promise<UnlistenFn>;
}

/** The stand-in transport planted for the preview page (see `seedBoardPreview`). */
let previewSeed: BoardTransport | null = null;

/**
 * Dev-only seam for the preview page: plant a stand-in transport so the
 * `plans` scene renders sample plan documents in a plain browser, where no
 * Tauri host exists. Ignored inside the desktop app — `createBoardTransport`
 * only consults the seed when there is no Tauri runtime, so production can
 * never serve planted data.
 */
export function seedBoardPreview(transport: BoardTransport | null): void {
  previewSeed = transport;
}

export function createTauriBoardTransport(bindings: BoardTransportBindings): BoardTransport {
  return {
    snapshot: () => bindings.invoke<BoardSnapshot>("board_snapshot"),
    document: (planId) => bindings.invoke<string>("board_plan_document", { planId }),
    onChanged: (handler) => bindings.listen(BOARD_EVENT_CHANNEL, () => handler()),
  };
}

/** The native board transport, or the planted preview seed in a plain browser. */
export function createBoardTransport(bindings?: BoardTransportBindings): BoardTransport | null {
  if (bindings) return createTauriBoardTransport(bindings);
  if (isTauriRuntime()) return createTauriBoardTransport({ invoke, listen });
  return previewSeed;
}
