import { createBoardTransport, type BoardTransport } from "../boardEngine";
import { boardFromSnapshot } from "../boardState";
import { probeMnemo2Host } from "../runtime";
import { app } from "./core";

let boardTransport: BoardTransport | null = null;
let boardUnlisten: (() => void) | null = null;
let boardToken = 0;
let boardSyncTimer: ReturnType<typeof setTimeout> | null = null;

/** Refetch the plan board; failures keep the last board (it is only a view). */
export async function syncBoard(): Promise<void> {
  const transport = boardTransport;
  if (!transport) return;
  const token = boardToken;
  try {
    const snapshot = await transport.snapshot();
    if (token !== boardToken) return;
    app.update((state) => ({ ...state, board: boardFromSnapshot(snapshot) }));
  } catch {
    // The board is derived state; the next change event refetches it.
  }
}

export function scheduleBoardSync(): void {
  if (boardSyncTimer || !boardTransport) return;
  boardSyncTimer = setTimeout(() => {
    boardSyncTimer = null;
    void syncBoard();
  }, 60);
}

export async function disconnectBoard(): Promise<void> {
  boardToken += 1;
  if (boardSyncTimer) {
    clearTimeout(boardSyncTimer);
    boardSyncTimer = null;
  }
  const unlisten = boardUnlisten;
  boardUnlisten = null;
  unlisten?.();
  boardTransport = null;
}

export async function connectBoard(): Promise<void> {
  const transport = createBoardTransport();
  if (!transport) return;
  if (!(await probeMnemo2Host())) return;
  await disconnectBoard();
  const token = boardToken;
  boardTransport = transport;
  try {
    const unlisten = await transport.onChanged(() => {
      if (token === boardToken) scheduleBoardSync();
    });
    if (token !== boardToken) {
      unlisten();
      return;
    }
    boardUnlisten = unlisten;
  } catch {
    // Without the event the board still loads on open and on team changes.
  }
  await syncBoard();
}
