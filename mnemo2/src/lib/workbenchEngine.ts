import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";
import type { DiffSnapshot } from "./types";

export interface WorkbenchTransportBindings {
  invoke: typeof invoke;
}

/** Read-only native data for the workbench; no agent tool calls are involved. */
export interface WorkbenchTransport {
  diff(workerId: string): Promise<DiffSnapshot>;
  screenshot(workerId: string, path: string): Promise<string>;
}

export function createTauriWorkbenchTransport(
  bindings: WorkbenchTransportBindings,
): WorkbenchTransport {
  return {
    diff: (workerId) => bindings.invoke<DiffSnapshot>("workbench_diff", { workerId }),
    screenshot: (workerId, path) =>
      bindings.invoke<string>("workbench_screenshot", { workerId, path }),
  };
}

export function createWorkbenchTransport(
  bindings?: WorkbenchTransportBindings,
): WorkbenchTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriWorkbenchTransport(bindings ?? { invoke });
}
