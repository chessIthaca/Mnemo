import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";

/** The channel asking the frontend to open a registered project. */
export const EXTERNAL_OPEN_CHANNEL = "mnemo2://external-open-request";

/** The channel reporting an external lead attaching to or detaching from a project. */
export const EXTERNAL_LEAD_CHANNEL = "mnemo2://external-lead";

/** Claude Code (over MCP) asks Mnemo2 to open the project at `root`. */
export interface ExternalOpenRequest {
  root: string;
}

/** An external lead attached to or detached from the project at `root`. */
export interface ExternalLeadEvent {
  attached: boolean;
  root: string | null;
}

/** The native host's current external-lead state. */
export interface ExternalLeadStatus {
  attached: boolean;
  root: string | null;
  since_ms: number | null;
}

/** What the UI knows about the open project's external lead. */
export interface ExternalLeadState {
  attached: boolean;
  root: string | null;
  sinceMs: number | null;
}

export interface ExternalLeadTransportBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

export interface ExternalLeadTransport {
  /** The `--project` the app was started with, once; `null` afterwards. */
  startupProject(): Promise<string | null>;
  status(): Promise<ExternalLeadStatus>;
  onOpenRequest(handler: (request: ExternalOpenRequest) => void): Promise<UnlistenFn>;
  onChanged(handler: (event: ExternalLeadEvent) => void): Promise<UnlistenFn>;
  /** Tell the host why a requested project could not be opened. */
  reportOpenFailure(root: string, reason: string): Promise<void>;
}

export function createTauriExternalLeadTransport(
  bindings: ExternalLeadTransportBindings,
): ExternalLeadTransport {
  return {
    startupProject: () => bindings.invoke<string | null>("mnemo2_startup_project"),
    status: () => bindings.invoke<ExternalLeadStatus>("external_lead_status"),
    onOpenRequest: (handler) =>
      bindings.listen<ExternalOpenRequest>(EXTERNAL_OPEN_CHANNEL, (event) => handler(event.payload)),
    onChanged: (handler) =>
      bindings.listen<ExternalLeadEvent>(EXTERNAL_LEAD_CHANNEL, (event) => handler(event.payload)),
    reportOpenFailure: (root, reason) => bindings.invoke<void>("external_open_failed", { root, reason }),
  };
}

/** The native external-lead transport, or `null` in a browser-only session. */
export function createExternalLeadTransport(
  bindings?: ExternalLeadTransportBindings,
): ExternalLeadTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriExternalLeadTransport(bindings ?? { invoke, listen });
}

/** No external lead: the built-in lead leads. */
export function noExternalLead(): ExternalLeadState {
  return { attached: false, root: null, sinceMs: null };
}

/**
 * A comparable form of a project path: the Windows `\\?\` prefix is dropped,
 * separators are `/`, trailing slashes are gone, and drive-letter or UNC
 * paths (Windows file systems ignore case) are lower-cased.
 */
export function normalizeProjectPath(path: string): string {
  let value = path.trim();
  if (value.startsWith("\\\\?\\UNC\\")) value = `\\\\${value.slice(8)}`;
  else if (value.startsWith("\\\\?\\")) value = value.slice(4);
  value = value.replace(/\\/g, "/").replace(/\/+$/, "");
  if (value === "") return "/";
  return /^[a-z]:/i.test(value) || value.startsWith("//") ? value.toLowerCase() : value;
}

export function sameProjectPath(a: string, b: string): boolean {
  return normalizeProjectPath(a) === normalizeProjectPath(b);
}

/** The external-lead state for the project at `projectPath` from a native status. */
export function externalLeadFromStatus(
  status: ExternalLeadStatus,
  projectPath: string,
): ExternalLeadState {
  if (!status.attached || !status.root || !sameProjectPath(status.root, projectPath)) {
    return noExternalLead();
  }
  return { attached: true, root: status.root, sinceMs: status.since_ms };
}

/**
 * Apply one attach/detach event to the open project's external lead. Events
 * for another root are ignored; a detach without a root clears it (nothing is
 * attached anywhere), an attach without one cannot be placed and is ignored.
 */
export function applyExternalLeadEvent(
  current: ExternalLeadState,
  event: ExternalLeadEvent,
  projectPath: string | null,
  nowMs: number = Date.now(),
): ExternalLeadState {
  if (projectPath === null) return current;
  if (event.root === null) return event.attached ? current : noExternalLead();
  if (!sameProjectPath(event.root, projectPath)) return current;
  if (!event.attached) return noExternalLead();
  if (current.attached && current.root !== null && sameProjectPath(current.root, event.root)) {
    return current;
  }
  return { attached: true, root: event.root, sinceMs: nowMs };
}
