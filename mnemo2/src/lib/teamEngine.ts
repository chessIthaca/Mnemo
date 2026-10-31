import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";
import type {
  TeamEvent,
  TeamFinding,
  TeamMessage,
  TeamReviewEvidence,
  TeamSnapshot,
  TeamWorkerRecord,
  WorkerDeletionPreview,
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
  dispatch(
    brief: string,
    planId: string,
    /** 1-based board step. */
    step: number,
    model?: string | null,
    workerId?: string | null,
    reasoningEffort?: string | null,
  ): Promise<TeamWorkerRecord>;
  message(workerId: string, text: string): Promise<TeamWorkerRecord>;
  pause(workerId: string): Promise<TeamWorkerRecord>;
  interrupt(workerId: string): Promise<TeamWorkerRecord>;
  stop(workerId: string): Promise<TeamWorkerRecord>;
  resume(workerId: string): Promise<TeamWorkerRecord>;
  /** Move a worker to another model (null effort = the model's default). */
  setModel(workerId: string, model: string, reasoningEffort: string | null): Promise<TeamWorkerRecord>;
  compact(workerId: string): Promise<TeamWorkerRecord>;
  review(
    workerId: string,
    summary: string,
    findings: TeamFinding[],
    evidence: TeamReviewEvidence,
  ): Promise<TeamWorkerRecord>;
  recordCheck(
    workerId: string,
    name: string,
    status: "passed" | "failed",
    detail?: string | null,
  ): Promise<TeamWorkerRecord>;
  resolveFinding(
    workerId: string,
    findingId: string,
    status: "fixed" | "wontfix",
    evidence?: string | null,
    justification?: string | null,
  ): Promise<TeamWorkerRecord>;
  reviewFix(
    workerId: string,
    findingId: string,
    path: string,
    oldString: string,
    newString: string,
  ): Promise<TeamWorkerRecord>;
  routeFix(workerId: string, findingId: string): Promise<TeamWorkerRecord>;
  merge(workerId: string): Promise<TeamWorkerRecord>;
  revertMerge(workerId: string): Promise<TeamWorkerRecord>;
  stopAll(): Promise<void>;
  setSlots(slots: number): Promise<TeamSnapshot>;
  deletePreview(workerIds: string[]): Promise<WorkerDeletionPreview[]>;
  deleteWorkers(workerIds: string[], discardWork: boolean): Promise<TeamSnapshot>;
  messages(): Promise<TeamMessage[]>;
  onEvent(handler: (event: TeamEvent) => void): Promise<UnlistenFn>;
}

/** Build the native team transport from real Tauri bindings. */
export function createTauriTeamTransport(
  bindings: TeamTransportBindings,
): TeamTransport {
  return {
    snapshot: () => bindings.invoke<TeamSnapshot>("team_snapshot"),
    dispatch: (brief, planId, step, model, workerId, reasoningEffort) =>
      bindings.invoke<TeamWorkerRecord>("team_dispatch", {
        brief,
        planId,
        step,
        model: model ?? null,
        reasoningEffort: reasoningEffort ?? null,
        workerId: workerId ?? null,
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
    setModel: (workerId, model, reasoningEffort) =>
      bindings.invoke<TeamWorkerRecord>("team_set_model", { workerId, model, reasoningEffort }),
    compact: (workerId) =>
      bindings.invoke<TeamWorkerRecord>("team_compact", { workerId }),
    review: (workerId, summary, findings, evidence) =>
      bindings.invoke<TeamWorkerRecord>("team_review", {
        workerId,
        summary,
        findings,
        evidence,
      }),
    recordCheck: (workerId, name, status, detail) =>
      bindings.invoke<TeamWorkerRecord>("team_record_check", {
        workerId,
        name,
        status,
        detail: detail ?? null,
      }),
    resolveFinding: (workerId, findingId, status, evidence, justification) =>
      bindings.invoke<TeamWorkerRecord>("team_resolve_finding", {
        workerId,
        findingId,
        status,
        evidence: evidence ?? null,
        justification: justification ?? null,
      }),
    reviewFix: (workerId, findingId, path, oldString, newString) =>
      bindings.invoke<TeamWorkerRecord>("team_review_fix", {
        workerId,
        findingId,
        path,
        oldString,
        newString,
      }),
    routeFix: (workerId, findingId) =>
      bindings.invoke<TeamWorkerRecord>("team_route_fix", { workerId, findingId }),
    merge: (workerId) =>
      bindings.invoke<TeamWorkerRecord>("team_merge", { workerId }),
    revertMerge: (workerId) =>
      bindings.invoke<TeamWorkerRecord>("team_revert_merge", { workerId }),
    stopAll: () => bindings.invoke<void>("team_stop_all"),
    setSlots: (slots) => bindings.invoke<TeamSnapshot>("team_set_slots", { slots }),
    deletePreview: (workerIds) =>
      bindings.invoke<WorkerDeletionPreview[]>("team_delete_preview", { workerIds }),
    deleteWorkers: (workerIds, discardWork) =>
      bindings.invoke<TeamSnapshot>("team_delete", { workerIds, discardWork }),
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
