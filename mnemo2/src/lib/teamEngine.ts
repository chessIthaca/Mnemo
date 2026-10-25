import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";
import type {
  TeamEvent,
  TeamMessage,
  TeamSnapshot,
  TeamWorkerRecord,
} from "./teamState";

/** The channel emitted by the native team runtime. */
export const TEAM_EVENT_CHANNEL = "mnemo2://team-event";

export interface TeamTransportBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

/** The frontend's typed view of the native team commands. */
export interface TeamTransport {
  snapshot(): Promise<TeamSnapshot>;
  dispatch(brief: string, model?: string | null): Promise<TeamWorkerRecord>;
  message(workerId: string, text: string): Promise<TeamWorkerRecord>;
  pause(workerId: string): Promise<TeamWorkerRecord>;
  interrupt(workerId: string): Promise<TeamWorkerRecord>;
  stop(workerId: string): Promise<TeamWorkerRecord>;
  resume(workerId: string): Promise<TeamWorkerRecord>;
  messages(): Promise<TeamMessage[]>;
  onEvent(handler: (event: TeamEvent) => void): Promise<UnlistenFn>;
}

/** Build the native team transport from real Tauri bindings. */
export function createTauriTeamTransport(
  bindings: TeamTransportBindings,
): TeamTransport {
  return {
    snapshot: () => bindings.invoke<TeamSnapshot>("team_snapshot"),
    dispatch: (brief, model) =>
      bindings.invoke<TeamWorkerRecord>("team_dispatch", {
        brief,
        model: model ?? null,
      }),
    message: (workerId, text) =>
      bindings.invoke<TeamWorkerRecord>("team_message", { workerId, text }),
    pause: (workerId) =>
      bindings.invoke<TeamWorkerRecord>("team_pause", { workerId }),
    interrupt: (workerId) =>
      bindings.invoke<TeamWorkerRecord>("team_interrupt", { workerId }),
    stop: (workerId) =>
      bindings.invoke<TeamWorkerRecord>("team_stop", { workerId }),
    resume: (workerId) =>
      bindings.invoke<TeamWorkerRecord>("team_resume", { workerId }),
    messages: () => bindings.invoke<TeamMessage[]>("team_messages"),
    onEvent: (handler) =>
      bindings.listen<TeamEvent>(TEAM_EVENT_CHANNEL, (event) =>
        handler(event.payload),
      ),
  };
}

/**
 * Create the native team transport, or `null` in a browser-only Vite session.
 * Tests may pass explicit bindings.
 */
export function createTeamTransport(
  bindings?: TeamTransportBindings,
): TeamTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriTeamTransport(bindings ?? { invoke, listen });
}
