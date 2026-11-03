import type {
  Finding,
  InboxEntry,
  LeadAgentEvent,
  LeadPendingInteraction,
  LeadTranscript,
  ProjectUsage,
  ReviewEvidence,
  ReviewRisk,
  WorkerKind,
  WorkerRouting,
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

/** The review evidence object as serialized by the native host. */
export interface TeamReviewEvidence {
  unanchored_pass?: boolean | null;
  intent?: string | null;
  execution?: string | null;
  integration?: string | null;
  test_quality?: string | null;
  residual_risk?: string | null;
  independent_reviewer_id?: string | null;
  audit_adjudication?: string | null;
}

export interface TeamReviewRecord {
  summary: string;
  tip: string;
  reviewed_at_ms: number;
  evidence?: TeamReviewEvidence | null;
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

/** Who sent a message to a worker, as the host serializes it. */
export type TeamInboxSender = "external_lead" | "lead" | "user" | "mnemo";

/** The form a message to a worker was delivered in, as the host serializes it. */
export type TeamInboxKind = "prompt" | "steer" | "brief" | "fix";

/** One durable message delivered to a worker, as the host serializes it. */
export interface TeamInboxEntry {
  sender: TeamInboxSender;
  kind: TeamInboxKind;
  text: string;
  at_ms: number;
}

export interface TeamWorkerRecord {
  id: string;
  name: string;
  brief: string;
  model: string | null;
  model_id: string | null;
  reasoning_effort?: string | null;
  branch: string;
  worktree: string;
  status: TeamWorkerStatus;
  generation: number;
  transcript: LeadTranscript;
  pending: LeadPendingInteraction | null;
  used_tokens: number;
  max_tokens: number;
  brief_sent?: boolean;
  compacting?: boolean;
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
  /** The board plan this task belongs to, and its 0-based step. */
  plan_id?: string | null;
  plan_step?: number | null;
  /** The model tier the lead dispatched by, when it used one. */
  tier?: string | null;
  merge: TeamMergeRecord | null;
  error: string | null;
  /** Serde-defaulted on the host: absent means an implementation worker. */
  kind?: string | null;
  /** Serde-defaulted on the host: absent means a standard-risk review. */
  review_risk?: string | null;
  /** Set on audit workers: the implementation worker whose tip is inspected. */
  audit_target?: string | null;
  /** Set on audit workers: the exact implementation tip the audit inspected. */
  audit_tip?: string | null;
  /** The one dispatch-time routing decision, when the dispatch recorded one. */
  routing?: WorkerRouting | null;
  /**
   * Every message this worker received, oldest first: sender, delivery form,
   * text, time. Absent in ledgers saved before senders were recorded.
   */
  inbox?: TeamInboxEntry[];
}

/**
 * The durable per-project usage accumulator as serialized by the native host.
 *
 * `input_tokens` is uncached input only, `cached_tokens` is reported
 * separately, `output_tokens` is completion output, and `cost_micros` covers
 * priced usage only — `unpriced_models` names the models whose usage has no
 * configured or vendor price, so the UI can mark the total as partial.
 */
export interface ProjectUsageView {
  input_tokens: number;
  cached_tokens: number;
  output_tokens: number;
  cost_micros: number;
  unpriced_models: string[];
}

/** What deleting one worker removes (`team_delete_preview`). */
export interface WorkerDeletionPreview {
  worker_id: string;
  name: string;
  branch: string;
  /** The session is live; deleting stops it first. */
  live: boolean;
  /** The checkout still exists and will be removed. */
  checkout: boolean;
  /** That checkout has uncommitted changes. */
  uncommitted: boolean;
  /** Commits on the branch that the lead branch does not contain. */
  unmerged_commits: number;
  /** Another record uses the same branch, so it is kept. */
  shared_branch: boolean;
  /** Why this worker cannot be deleted now. */
  blocked: string | null;
  /**
   * The merged worker that landed this same plan step through its own
   * branch, when one exists: the unmerged commits counted above are this
   * worker's own superseded copy, not work lost forever. Null when no other
   * worker landed the step, or when the host predates the field.
   */
  superseded_by: string | null;
}

/** Whether deleting this worker discards work the lead branch lacks. */
export function deletionDiscardsWork(preview: WorkerDeletionPreview): boolean {
  return preview.uncommitted || preview.unmerged_commits > 0;
}

/**
 * Map one deletion preview from the host's wire shape to the UI shape.
 *
 * `superseded_by` is newer than the preview itself: a host that predates the
 * field sends no such key, which reads here as null — no other worker landed
 * this preview's step, so the work its delete discards is genuinely lost.
 */
export function deletionPreviewFromWire(raw: WorkerDeletionPreview): WorkerDeletionPreview {
  return {
    ...raw,
    superseded_by: typeof raw.superseded_by === "string" && raw.superseded_by.length > 0
      ? raw.superseded_by
      : null,
  };
}

/** One worker's outcome of a completed deletion batch (`team_delete`). */
export interface WorkerDeletionResult {
  worker_id: string;
  name: string;
  /** Null exactly when the worker was deleted. */
  error: string | null;
}

/** `team_delete`'s return: per-worker results plus the team snapshot. */
export interface TeamDeleteOutcome {
  results: WorkerDeletionResult[];
  snapshot: TeamSnapshot;
}

export interface TeamSnapshot {
  workers: TeamWorkerRecord[];
  slots: number;
  active: number;
  /** Durable usage for the whole project, including lead and deleted workers. */
  usage: ProjectUsageView;
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
  compacting: boolean;
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
  kind: WorkerKind;
  reviewRisk: ReviewRisk;
  auditTarget: string | null;
  auditTip: string | null;
  routing: WorkerRouting | null;
}

export interface TeamViewState {
  workers: TeamWorkerView[];
  slots: number;
  active: number;
  usage: ProjectUsage;
  messages: TeamMessage[];
}

/** A zeroed usage aggregate for a project that has recorded nothing yet. */
export function emptyProjectUsage(): ProjectUsage {
  return {
    inputTokens: 0,
    cachedTokens: 0,
    outputTokens: 0,
    costMicros: 0,
    unpricedModels: [],
  };
}

/** A zeroed usage record in the host's wire shape, for fixtures and defaults. */
export function emptyProjectUsageView(): ProjectUsageView {
  return {
    input_tokens: 0,
    cached_tokens: 0,
    output_tokens: 0,
    cost_micros: 0,
    unpriced_models: [],
  };
}

function usageCount(value: unknown): number {
  const count = typeof value === "number" ? value : Number(value);
  return Number.isFinite(count) && count > 0 ? Math.trunc(count) : 0;
}

/**
 * Normalize the host's durable usage record into the UI view.
 *
 * Counts are truncated to non-negative integers with saturating semantics so
 * a malformed or older record can never render a negative cost, and a host
 * that predates the accumulator simply reads as zero usage.
 */
export function projectUsageFrom(record: ProjectUsageView | null | undefined): ProjectUsage {
  if (!record || typeof record !== "object") return emptyProjectUsage();
  const models = Array.isArray(record.unpriced_models)
    ? record.unpriced_models.filter((model): model is string => typeof model === "string")
    : [];
  return {
    inputTokens: usageCount(record.input_tokens),
    cachedTokens: usageCount(record.cached_tokens),
    outputTokens: usageCount(record.output_tokens),
    costMicros: usageCount(record.cost_micros),
    unpricedModels: [...new Set(models)],
  };
}

export const initialTeamState: TeamViewState = {
  workers: [],
  slots: 0,
  active: 0,
  usage: emptyProjectUsage(),
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

/**
 * Map one durable inbox entry from the host's wire shape to the UI shape.
 *
 * The sender and kind are written by the host, but a record saved by an older
 * host — or read by a newer frontend — could carry a value this build does not
 * know; those degrade to Mnemo2/prompt rather than crashing the ledger.
 */
export function inboxFromWire(entry: TeamInboxEntry): InboxEntry {
  const senders: readonly TeamInboxSender[] = ["external_lead", "lead", "user", "mnemo"];
  const kinds: readonly TeamInboxKind[] = ["prompt", "steer", "brief", "fix"];
  return {
    sender: senders.includes(entry.sender) ? entry.sender : "mnemo",
    kind: kinds.includes(entry.kind) ? entry.kind : "prompt",
    text: typeof entry.text === "string" ? entry.text : "",
    atMs: entry.at_ms,
  };
}

/**
 * Normalize the host's worker kind.
 *
 * The field is serde-defaulted on the host, so a record written before audit
 * workers existed (or any unrecognized value) reads as an implementation
 * worker; only an explicit `"audit"` marks a read-only reviewer.
 */
export function workerKindFrom(value: string | null | undefined): WorkerKind {
  return value === "audit" ? "audit" : "implementation";
}

/**
 * Normalize the host's review risk.
 *
 * Absent or unrecognized values read as `"standard"`, matching the host's
 * serde default, so legacy ledgers never demand an audit they never had.
 */
export function reviewRiskFrom(value: string | null | undefined): ReviewRisk {
  return value === "high" ? "high" : "standard";
}

/**
 * Normalize a host review-evidence object into the UI shape.
 *
 * Returns `undefined` for a review recorded before structured evidence
 * existed, which is a distinct state from an evidence object with an empty
 * assessment: the UI must ask for a fresh review rather than render blanks.
 */
export function reviewEvidenceFrom(
  evidence: TeamReviewEvidence | null | undefined,
): ReviewEvidence | undefined {
  if (!evidence || typeof evidence !== "object") return undefined;
  const text = (value: string | null | undefined) => (typeof value === "string" ? value : "");
  return {
    unanchoredPass: evidence.unanchored_pass === true,
    intent: text(evidence.intent),
    execution: text(evidence.execution),
    integration: text(evidence.integration),
    testQuality: text(evidence.test_quality),
    residualRisk: text(evidence.residual_risk),
    independentReviewerId: evidence.independent_reviewer_id ?? undefined,
    auditAdjudication: evidence.audit_adjudication ?? undefined,
  };
}

/**
 * Serialize the UI evidence shape into the host's `ReviewEvidence` wire
 * object for `team_review`.
 *
 * The base six fields are always sent because the host requires them; the
 * optional audit fields are omitted when absent, matching the host's
 * `skip_serializing_if` on `Option<String>`. Callers pass the lead's real
 * assessments, never a placeholder: the host rejects incomplete evidence.
 */
export function reviewEvidenceToWire(evidence: ReviewEvidence): TeamReviewEvidence {
  const wire: TeamReviewEvidence = {
    unanchored_pass: evidence.unanchoredPass,
    intent: evidence.intent,
    execution: evidence.execution,
    integration: evidence.integration,
    test_quality: evidence.testQuality,
    residual_risk: evidence.residualRisk,
  };
  if (evidence.independentReviewerId !== undefined) {
    wire.independent_reviewer_id = evidence.independentReviewerId;
  }
  if (evidence.auditAdjudication !== undefined) {
    wire.audit_adjudication = evidence.auditAdjudication;
  }
  return wire;
}

export function findingToWire(finding: Finding): TeamFinding {
  return {
    ...finding,
    fix_commit: finding.fixCommit ?? null,
    evidence: finding.evidence ?? null,
    justification: finding.justification ?? null,
  };
}

/**
 * Whether a worker's pending interaction actually waits on a person. A
 * worker's host-tool call (`message_parent`) is answered by the runtime
 * itself, so it is progress, never a reason to ask for attention.
 */
export function waitsOnUser(pending: LeadPendingInteraction | null | undefined): boolean {
  return pending != null && pending.kind !== "host_tool_call";
}

/**
 * A worker's display name. The lead names its workers by id (`ghostai0001`);
 * records saved before that stored "Worker <first 8 chars>", which cut such a
 * name short. Only a generated id (20 hex digits) keeps the short form.
 */
export function workerDisplayName(id: string, name: string): string {
  const generated = /^[0-9a-f]{20}$/.test(id);
  return !generated && name === `Worker ${id.slice(0, 8)}` ? id : name;
}

function toView(record: TeamWorkerRecord): TeamWorkerView {
  return {
    id: record.id,
    name: workerDisplayName(record.id, record.name),
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
    compacting: record.compacting === true,
    totalTokens: record.total_tokens,
    promptTokens: record.prompt_tokens,
    completionTokens: record.completion_tokens,
    cachedTokens: record.cached_tokens,
    requests: record.requests,
    costMicros: record.cost_micros,
    elapsedMs: record.elapsed_ms,
    turns: record.turns,
    lastActivityMs: record.last_activity_ms,
    needsYou: record.status === "needs_you" || waitsOnUser(record.pending),
    pending: record.pending,
    checks: record.checks,
    findings: record.findings.map(findingFromWire),
    review: record.review,
    fixerFindingId: record.fixer_finding_id,
    merge: record.merge,
    error: record.error,
    kind: workerKindFrom(record.kind),
    reviewRisk: reviewRiskFrom(record.review_risk),
    auditTarget: record.audit_target ?? null,
    auditTip: record.audit_tip ?? null,
    routing: record.routing ?? null,
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
    usage: projectUsageFrom(snapshot.usage),
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
    next.compacting = false;
  }
  const payload = event.event.event;
  switch (payload.kind) {
    case "started":
      if (!["paused", "interrupted", "stopped", "failed", "merged", "cancelled"].includes(next.status)) {
        next.status = "working";
        next.compacting = false;
      }
      break;
    case "compact_started":
      next.status = "working";
      next.compacting = true;
      next.needsYou = false;
      next.error = null;
      break;
    case "compacted":
      if (next.compacting) {
        next.compacting = false;
        next.status = next.pending ? "needs_you" : "idle";
        next.needsYou = next.pending != null;
        next.error = null;
      }
      break;
    case "text_delta":
      next.text += payload.text ?? "";
      break;
    case "tool_execution_start":
      // The worker view carries no per-call timers (tool cards and their
      // elapsed suffixes live in the lead thread); the event must never
      // disturb worker status, text, or token counters.
      break;
    case "approval_request":
    case "user_question":
      next.status = "needs_you";
      next.needsYou = true;
      break;
    case "finished":
      if (!["completed", "paused", "interrupted", "stopped", "failed", "merged", "cancelled"].includes(next.status)) {
        next.status = "completed";
      }
      next.needsYou = false;
      next.pending = null;
      next.compacting = false;
      break;
    case "exited":
      if (!["completed", "idle", "needs_you", "paused", "interrupted", "stopped", "failed", "merged", "cancelled"].includes(next.status)) {
        next.status = "interrupted";
        next.error = "the worker session exited before completing its turn";
      }
      next.needsYou = next.status === "needs_you" || next.pending != null;
      next.pending = null;
      next.compacting = false;
      break;
    case "parked":
      next.compacting = false;
      if (!terminal(next.status)) {
        next.status = "needs_you";
        next.needsYou = true;
      }
      break;
    case "error":
      if (next.compacting) {
        next.compacting = false;
        next.status = next.pending ? "needs_you" : "idle";
        next.needsYou = next.pending != null;
        next.error = payload.retrying ? null : (payload.error ?? payload.message ?? "compaction failed");
      } else if (!payload.retrying) {
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
