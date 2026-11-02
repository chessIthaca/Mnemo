import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";
import type {
  LeadAnswerInput,
  LeadEvent,
  LeadSnapshot,
} from "./types";

export { isTauriRuntime };

/** The one event channel emitted by the Mnemo2 host. */
export const LEAD_EVENT_CHANNEL = "mnemo2://lead-event";

export interface LeadOpenOptions {
  workspaceRoot: string;
  configDir?: string | null;
  model?: string | null;
  allowInstanceConflict?: boolean;
}

export type LeadApprovalDecision = "approve" | "deny" | "deny_all";

/** The frontend's typed view of the native lead commands. */
export interface LeadTransport {
  probe(): Promise<boolean>;
  open(options: LeadOpenOptions): Promise<LeadSnapshot>;
  snapshot(): Promise<LeadSnapshot>;
  /**
   * Submit text (and optional image data URLs) to the lead. The host turns a
   * submission on a busy turn into a steer instead of starting a new prompt.
   */
  prompt(text: string, images?: string[]): Promise<void>;
  interrupt(): Promise<void>;
  /** Compact the lead's context between turns; the session stays open. */
  compact(): Promise<void>;
  setModel(spec: string, reasoningEffort?: string | null): Promise<LeadSnapshot>;
  approve(requestId: string, decision: LeadApprovalDecision): Promise<void>;
  answer(questionId: string, answer: LeadAnswerInput): Promise<void>;
  close(): Promise<void>;
  onEvent(handler: (event: LeadEvent) => void): Promise<UnlistenFn>;
}

export interface LeadTransportBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

/** Build the native transport from the real Tauri bindings. */
/** Build the native transport from the real Tauri bindings. */
export function createTauriLeadTransport(bindings: LeadTransportBindings): LeadTransport {
  return {
    probe: () =>
      bindings
        .invoke<string>("mnemo2_host_ping")
        .then((reply) => reply === "pong")
        .catch(() => false),
    open: (options) =>
      bindings.invoke<LeadSnapshot>("lead_open", {
        workspaceRoot: options.workspaceRoot,
        configDir: options.configDir ?? null,
        model: options.model ?? null,
        ...(options.allowInstanceConflict === undefined
          ? {}
          : { allowInstanceConflict: options.allowInstanceConflict }),
      }),
    snapshot: () => bindings.invoke<LeadSnapshot>("lead_snapshot"),
    prompt: (text, images) =>
      bindings.invoke<void>(
        "lead_prompt",
        images && images.length > 0 ? { text, images } : { text },
      ),
    interrupt: () => bindings.invoke<void>("lead_interrupt"),
    compact: () => bindings.invoke<void>("lead_compact"),
    setModel: (spec, reasoningEffort) =>
      bindings.invoke<LeadSnapshot>("lead_set_model", {
        spec,
        reasoningEffort: reasoningEffort ?? null,
      }),
    approve: (requestId, decision) =>
      bindings.invoke<void>("lead_approve", { requestId, decision }),
    answer: (questionId, answer) =>
      bindings.invoke<void>("lead_answer", { questionId, answer }),
    close: () => bindings.invoke<void>("lead_close"),
    onEvent: (handler) =>
      bindings.listen<LeadEvent>(LEAD_EVENT_CHANNEL, (event) => handler(event.payload)),
  };
}

/**
 * Create the native lead transport, or `null` in a browser-only Vite session.
 * Tests may pass explicit bindings.
 */
export function createLeadTransport(
  bindings?: LeadTransportBindings,
): LeadTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriLeadTransport(bindings ?? { invoke, listen });
}
