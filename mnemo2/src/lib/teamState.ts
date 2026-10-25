import type {
  LeadAgentEvent,
  LeadPendingInteraction,
  LeadTranscript,
} from "./types";

/** The real worker lifecycle reported by the native team runtime. */
export type TeamWorkerStatus =
  | "dispatching"
  | "working"
  | "needs_you"
  | "idle"
  | "paused"
  | "interrupted"
  | "completed"
  | "stopped"
  | "failed"
  | (string & {});

export interface TeamWorkerRecord {
  id: string;
  name: string;
  brief: string;
  model: string | null;
  model_id: string | null;
  branch: string;
  worktree: string;
  status: TeamWorkerStatus;
  generation: number;
  transcript: LeadTranscript;
  pending: LeadPendingInteraction | null;
  used_tokens: number;
  max_tokens: number;
  checks: { name: string; status: string; detail?: string | null }[];
  error: string | null;
}

export interface TeamSnapshot {
  workers: TeamWorkerRecord[];
  slots: number;
  active: number;
}

export interface TeamMessage {
  worker_id: string;
  text: string;
  at_ms: number;
}

export interface TeamSessionEvent {
  agent: number;
  event: LeadAgentEvent;
  rates?: unknown;
}

export interface TeamEvent {
  worker_id: string;
  generation: number;
  event: TeamSessionEvent;
}

/** The UI's real view of one worker. */
export interface TeamWorkerView {
  id: string;
  name: string;
  brief: string;
  model: string | null;
  modelId: string | null;
  branch: string;
  worktree: string;
  status: TeamWorkerStatus;
  generation: number;
  text: string;
  usedTokens: number;
  maxTokens: number;
  needsYou: boolean;
  pending: LeadPendingInteraction | null;
  error: string | null;
}

export interface TeamViewState {
  workers: TeamWorkerView[];
  slots: number;
  active: number;
  messages: TeamMessage[];
}

export const initialTeamState: TeamViewState = {
  workers: [],
  slots: 0,
  active: 0,
  messages: [],
};

function transcriptText(transcript: LeadTranscript | null | undefined): string {
  return (transcript?.messages ?? [])
    .filter((message) => message.role === "assistant")
    .map((message) => (typeof message.content === "string" ? message.content : ""))
    .join("");
}

function toView(record: TeamWorkerRecord): TeamWorkerView {
  return {
    id: record.id,
    name: record.name,
    brief: record.brief,
    model: record.model,
    modelId: record.model_id,
    branch: record.branch,
    worktree: record.worktree,
    status: record.status,
    generation: record.generation,
    text: transcriptText(record.transcript),
    usedTokens: record.used_tokens,
    maxTokens: record.max_tokens,
    needsYou: record.status === "needs_you" || record.pending != null,
    pending: record.pending,
    error: record.error,
  };
}

/** Replace the team view with a real snapshot. */
export function applyTeamSnapshot(
  snapshot: TeamSnapshot,
  messages: TeamMessage[] = [],
): TeamViewState {
  return {
    workers: snapshot.workers.map(toView),
    slots: snapshot.slots,
    active: snapshot.active,
    messages,
  };
}

function terminal(status: TeamWorkerStatus): boolean {
  return ["completed", "stopped", "paused", "interrupted", "failed"].includes(status);
}

/** Fold one generation-tagged team event into the view. */
export function applyTeamEvent(state: TeamViewState, event: TeamEvent): TeamViewState {
  const index = state.workers.findIndex((worker) => worker.id === event.worker_id);
  if (index === -1) return state;
  const current = state.workers[index];
  if (event.generation < current.generation) return state;

  const next: TeamWorkerView = { ...current, generation: event.generation };
  if (event.generation > current.generation) {
    next.status = "working";
    next.needsYou = false;
    next.pending = null;
  }
  const payload = event.event.event;
  switch (payload.kind) {
    case "started":
      next.status = "working";
      break;
    case "text_delta":
      next.text += payload.text ?? "";
      break;
    case "approval_request":
    case "user_question":
    case "host_tool_call":
      next.status = "needs_you";
      next.needsYou = true;
      break;
    case "finished":
    case "exited":
      next.status = "completed";
      next.needsYou = false;
      next.pending = null;
      break;
    case "parked":
      if (!terminal(next.status)) {
        next.status = "needs_you";
        next.needsYou = true;
      }
      break;
    case "error":
      if (!payload.retrying) {
        next.status = "failed";
        next.error = payload.error ?? payload.message ?? "worker failed";
      }
      break;
    case "model_changed":
      next.modelId = payload.model ?? next.modelId;
      break;
    case "context_usage":
      next.usedTokens = Number(payload.used ?? next.usedTokens);
      next.maxTokens = Number(payload.max ?? next.maxTokens);
      break;
    case "usage":
      next.usedTokens = Math.max(next.usedTokens, Number(payload.prompt_tokens ?? 0));
      break;
    default:
      break;
  }
  const workers = state.workers.slice();
  workers[index] = next;
  return { ...state, workers };
}

/** Replace the worker-to-lead message list. */
export function applyTeamMessages(
  state: TeamViewState,
  messages: TeamMessage[],
): TeamViewState {
  return { ...state, messages };
}
