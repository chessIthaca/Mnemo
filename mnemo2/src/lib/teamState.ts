import type {
  Finding,
  LeadAgentEvent,
  LeadPendingInteraction,
  LeadTranscript,
} from "./types";
import { messageText } from "./leadState";

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
  | "in_review"
  | "fixing"
  | "merging"
  | "merged"
  | "conflict"
  | (string & {});

export interface TeamWorkerCheck {
  name: string;
  status: string;
  detail?: string | null;
  tip: string;
}

export interface TeamReviewRecord {
  summary: string;
  tip: string;
  reviewed_at_ms: number;
}

export interface TeamMergeRecord {
  commit: string;
  worker_tip: string;
  branch: string;
  target: string;
  checks: string[];
  review_summary: string;
  files: number;
  additions: number;
  deletions: number;
  worker_model: string | null;
  worker_tokens: number;
  cost_micros: number | null;
  merged_at_ms: number;
  revert_commit?: string | null;
}

export interface TeamFinding {
  id: string;
  severity: "high" | "low";
  title: string;
  detail: string;
  file: string;
  line: number;
  status: "open" | "fixed" | "wontfix";
  fix_commit?: string | null;
  evidence?: string | null;
  justification?: string | null;
}

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
  token_budget: number;
  total_tokens: number;
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  requests: number;
  cost_micros: number | null;
  created_at_ms: number;
  elapsed_ms: number;
  turns: number;
  last_activity_ms: number;
  checks: TeamWorkerCheck[];
  findings: TeamFinding[];
  review: TeamReviewRecord | null;
  fixer_finding_id: string | null;
  merge: TeamMergeRecord | null;
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
  tokenBudget: number;
  totalTokens: number;
  promptTokens: number;
  completionTokens: number;
  cachedTokens: number;
  requests: number;
  costMicros: number | null;
  elapsedMs: number;
  turns: number;
  lastActivityMs: number;
  needsYou: boolean;
  pending: LeadPendingInteraction | null;
  checks: TeamWorkerCheck[];
  findings: Finding[];
  review: TeamReviewRecord | null;
  fixerFindingId: string | null;
  merge: TeamMergeRecord | null;
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
    .map((message) => messageText(message.content))
    .join("");
}

export function findingFromWire(finding: TeamFinding): Finding {
  return {
    ...finding,
    fixCommit: finding.fix_commit ?? undefined,
    evidence: finding.evidence ?? undefined,
    justification: finding.justification ?? undefined,
  };
}

export function findingToWire(finding: Finding): TeamFinding {
  return {
    ...finding,
    fix_commit: finding.fixCommit ?? null,
    evidence: finding.evidence ?? null,
    justification: finding.justification ?? null,
  };
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
    tokenBudget: record.token_budget,
    totalTokens: record.total_tokens,
    promptTokens: record.prompt_tokens,
    completionTokens: record.completion_tokens,
    cachedTokens: record.cached_tokens,
    requests: record.requests,
    costMicros: record.cost_micros,
    elapsedMs: record.elapsed_ms,
    turns: record.turns,
    lastActivityMs: record.last_activity_ms,
    needsYou: record.status === "needs_you" || record.pending != null,
    pending: record.pending,
    checks: record.checks,
    findings: record.findings.map(findingFromWire),
    review: record.review,
    fixerFindingId: record.fixer_finding_id,
    merge: record.merge,
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
  return [
    "completed",
    "stopped",
    "paused",
    "interrupted",
    "failed",
    "in_review",
    "merging",
    "merged",
    "conflict",
    "needs_you",
  ].includes(status);
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
    case "exited": {
      const budgetPaused = next.status === "needs_you" && next.error?.includes("token budget");
      next.status = budgetPaused ? "needs_you" : "completed";
      next.needsYou = budgetPaused === true;
      next.pending = null;
      break;
    }
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
    case "usage": {
      const prompt = Number(payload.prompt_tokens ?? 0);
      const completion = Number(payload.completion_tokens ?? 0);
      const cached = Number(payload.cached_tokens ?? 0);
      next.totalTokens += prompt + completion;
      next.promptTokens += prompt;
      next.completionTokens += completion;
      next.cachedTokens += cached;
      next.requests += 1;
      next.usedTokens = Math.max(next.usedTokens, prompt);
      break;
    }
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
