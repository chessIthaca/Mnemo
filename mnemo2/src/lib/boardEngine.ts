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
  onChanged(handler: () => void): Promise<UnlistenFn>;
}

export function createTauriBoardTransport(bindings: BoardTransportBindings): BoardTransport {
  return {
    snapshot: () => bindings.invoke<BoardSnapshot>("board_snapshot"),
    onChanged: (handler) => bindings.listen(BOARD_EVENT_CHANNEL, () => handler()),
  };
}

/** The native board transport, or `null` in a browser-only session (an empty board). */
export function createBoardTransport(bindings?: BoardTransportBindings): BoardTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriBoardTransport(bindings ?? { invoke, listen });
}
