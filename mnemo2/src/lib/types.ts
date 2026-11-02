import type { ExternalLeadState } from "./externalLeadEngine";
import type { IndexingSnapshot } from "./indexingEngine";
import type { BoardPlan } from "./boardState";
import type { WorkerDeletionPreview } from "./teamState";

export type Route = "start" | "lead" | "team" | "cost" | "graph" | "plans" | "memory";

export type WorkbenchTab = "browser" | "diff" | "plan";
export type TeamFilter = "all" | "needs" | "active" | "done";

export type WorkerStatus =
  | "dispatching"
  | "working"
  | "needs_you"
  | "idle"
  | "paused"
  | "interrupted"
  | "completed"
  | "stopped"
  | "in_review"
  | "fixing"
  | "merging"
  | "merged"
  | "conflict"
  | "failed"
  | "cancelled";

/** What a running worker is doing right now, derived from its streamed events. */
export type WorkerPhase = "starting" | "thinking" | "writing" | "tool" | "waiting";

/** The live activity of one running worker: its phase, and how long it has been going on. */
export interface WorkerActivity {
  phase: WorkerPhase;
  /** The tool the worker is running, when `phase === "tool"`. */
  toolName: string | null;
  /** When the current turn began (ms epoch); null until the turn's `started` event arrives. */
  turnStartedAtMs: number | null;
  /** When the current phase began (ms epoch). */
  phaseSinceMs: number;
}

export const workerStatusLabel: Record<WorkerStatus, string> = {
  dispatching: "Dispatching",
  working: "Working",
  needs_you: "Needs you",
  idle: "Idle",
  paused: "Paused",
  interrupted: "Interrupted",
  completed: "Completed",
  stopped: "Stopped",
  in_review: "In review",
  fixing: "Fixing",
  merging: "Merging",
  merged: "Landed",
  conflict: "Conflict",
  failed: "Failed",
  cancelled: "Cancelled",
};

/** A worker's status as shown: a landed merge that was later reverted reads "Reverted". */
export function workerStatusText(worker: Pick<Worker, "status" | "merge">): string {
  return worker.status === "merged" && worker.merge?.revertCommit ? "Reverted" : workerStatusLabel[worker.status];
}

export type StepStatus = "done" | "active" | "queued" | "failed";

export interface TimelineStep {
  id: string;
  label: string;
  detail: string;
  status: StepStatus;
  time?: string;
}

export type TaskWorkflowStepId = "brief" | "implement" | "verify" | "review" | "merge";
export type TaskWorkflowStepStatus = "done" | "active" | "pending" | "blocked";
export type TaskWorkflowState = "active" | "blocked" | "done" | "cancelled";

export interface TaskWorkflowStep {
  id: TaskWorkflowStepId;
  label: string;
  status: TaskWorkflowStepStatus;
  detail: string;
}

export interface TaskWorkflow {
  current: TaskWorkflowStepId;
  state: TaskWorkflowState;
  steps: TaskWorkflowStep[];
}

export interface Project {
  id: string;
  name: string;
  path: string;
  branch: string;
  leadModel: string;
  description: string;
  lastSession: string;
  workerSlots: number;
  available?: boolean;
  unfinishedWorkers?: number;
  reviewWorkers?: number;
  budgetSpent?: number;
  budgetLimit?: number;
}

export interface Check {
  name: string;
  status: "passed" | "running" | "failed";
  detail: string;
  tip?: string;
}

export interface Finding {
  id: string;
  severity: "high" | "low";
  title: string;
  detail: string;
  file: string;
  line: number;
  status: "open" | "fixed" | "wontfix";
  fixCommit?: string;
  evidence?: string;
  justification?: string;
}

/** How much independent scrutiny a change needs before it may merge. */
export type ReviewRisk = "standard" | "high";

/** Whether a worker implements the brief or audits another worker's branch. */
export type WorkerKind = "implementation" | "audit";

/**
 * The structured, intent-driven review the lead records with every review.
 *
 * Legacy review records carry no evidence at all. The UI treats that as
 * "recorded before structured evidence existed" (a fresh review is required
 * before merge) rather than as an empty pass.
 */
export interface ReviewEvidence {
  unanchoredPass: boolean;
  intent: string;
  execution: string;
  integration: string;
  testQuality: string;
  residualRisk: string;
  independentReviewerId?: string;
  auditAdjudication?: string;
}

/** The durable review of one exact worker tip. */
export interface ReviewRecord {
  summary: string;
  tip: string;
  reviewedAtMs: number;
  evidence?: ReviewEvidence;
}

export interface WorkerMessage {
  id: string;
  kind?: "message";
  /** "harness" marks a harness note injected into the worker's context. */
  from: "worker" | "lead" | "harness";
  body: string;
  time: string;
}

/** One item of a worker's thread: a Markdown message or a tool activity. */
export type WorkerThreadEntry = WorkerMessage | Extract<LeadEntry, { kind: "tool" }>;

export interface DiffStats {
  files: number;
  additions: number;
  deletions: number;
}

export interface MergeRecord {
  id: string;
  workerId: string;
  commit: string;
  branch: string;
  target: string;
  files: number;
  additions: number;
  deletions: number;
  checks: string[];
  reviewSummary: string;
  model: string;
  tokens: number;
  cost: number | null;
  time: string;
  revertCommit?: string;
}

/** Who sent a message to a worker: Claude Code through MCP, the built-in lead, the user in mission control, or Mnemo2 itself. */
export type InboxSender = "external_lead" | "lead" | "user" | "mnemo";

/** The form a message to a worker was delivered in: a fresh prompt, a mid-turn steer, a route-fix brief, or a fix request. */
export type InboxKind = "prompt" | "steer" | "brief" | "fix";

/** One message a worker received, from the durable inbox ledger. */
export interface InboxEntry {
  sender: InboxSender;
  kind: InboxKind;
  text: string;
  /** Epoch milliseconds — when the message was recorded. */
  atMs: number;
}

export interface Worker {
  id: string;
  name: string;
  initials: string;
  hue: number;
  task: string;
  summary: string;
  status: WorkerStatus;
  model: string;
  /** The `endpoint/model` spec the worker was given; null = the configured default. */
  modelSpec?: string | null;
  /** The model tier it was dispatched or moved by, when one was used. */
  tier?: string;
  /** The worker's chosen reasoning effort, when one was set. */
  reasoningEffort?: string;
  branch: string;
  worktree: string;
  files: string[];
  tokens: number;
  cost: number | null;
  elapsed: string;
  turns: number;
  compacting?: boolean;
  progress: number;
  lastActivity: string;
  /** When the worker was created (ms epoch); live rows tick their timer from this. */
  createdAtMs: number;
  /** What the worker is doing right now, while it runs; null when it is not running. */
  activity: WorkerActivity | null;
  needsYou: boolean;
  /** Implementation workers own the brief; audit workers only review one tip. */
  kind: WorkerKind;
  /** Risk assigned at dispatch; high-risk reviews need an independent audit. */
  reviewRisk: ReviewRisk;
  /** For an audit worker: the worker whose branch/tip it inspects. */
  auditTarget?: string;
  /** For an audit worker: the exact implementation tip it inspected. */
  auditTip?: string;
  fixerFindingId?: string;
  /** The board plan this worker's task belongs to. */
  planId?: string | null;
  /** The 0-based step of that plan. */
  planStep?: number | null;
  checks: Check[];
  findings: Finding[];
  review?: ReviewRecord;
  messages: WorkerThreadEntry[];
  /**
   * Every message this worker received, oldest first, from the durable
   * inbox ledger — who sent it, in what form, and when. Empty when the
   * ledger predates sender recording.
   */
  inbox: InboxEntry[];
  screenshots: string[];
  timeline: TimelineStep[];
  diff: DiffStats;
  merge?: MergeRecord;
}

/** The displayable result of one real tool call. */
export interface LeadToolResult {
  success: boolean;
  output: string;
  data?: unknown;
}

/** The unsent composer draft, kept in store state so navigation preserves it. */
export interface LeadDraft {
  text: string;
  images: string[];
}

export type LeadEntry =
  | {
      id: string;
      kind: "message";
      time: string;
      author: "lead" | "user";
      body: string;
      /** Attachment data URLs that accompanied a user message. */
      images?: string[];
    }
  | {
      id: string;
      kind: "tool";
      time: string;
      toolCallId: string;
      toolName: string;
      /** The streamed JSON argument string, shown verbatim as text. */
      arguments: string;
      /** Live output kept from a running tool (tail-bounded by the reducer). */
      liveOutput: string;
      /** The provider's stream index, used to route argument fragments. */
      streamIndex?: number;
      /** Observed tool-start time in Unix epoch milliseconds. */
      startedAtMs?: number;
      /** Observed result time in Unix epoch milliseconds. */
      endedAtMs?: number;
      result: LeadToolResult | null;
    }
  | {
      id: string;
      kind: "spawn";
      time: string;
      title: string;
      body: string;
      workerIds: string[];
    }
  | {
      id: string;
      kind: "team-message";
      time: string;
      /** When it happened (Unix ms), used to place it in the lead chat. */
      atMs?: number;
      workerId: string;
      /** The worker's name when this was recorded. */
      workerName?: string;
      body: string;
    }
  | {
      id: string;
      kind: "review";
      time: string;
      /** When it happened (Unix ms), used to place it in the lead chat. */
      atMs?: number;
      workerId: string;
      /** The worker's name when this was recorded. */
      workerName?: string;
      title: string;
      body: string;
    }
  | {
      id: string;
      kind: "merge";
      time: string;
      /** When it happened (Unix ms), used to place it in the lead chat. */
      atMs?: number;
      workerId: string;
      /** The worker's name when this was recorded. */
      workerName?: string;
      record: MergeRecord;
    };

export interface PlanItem {
  id: string;
  title: string;
  detail: string;
  owner: string;
  status: StepStatus;
}

export interface DiffLine {
  type: "add" | "remove" | "context" | "hunk";
  text: string;
  old_line?: number | null;
  new_line?: number | null;
}

export interface DiffFile {
  path: string;
  old_path?: string | null;
  additions: number;
  deletions: number;
  lines: DiffLine[];
  binary?: boolean;
  truncated?: boolean;
  sensitive?: boolean;
}

/** A real Git delta, including totals for content outside the bounded preview. */
export interface DiffSnapshot {
  worker_id: string;
  branch: string;
  target: string;
  base: string;
  tip: string;
  files: DiffFile[];
  total_files: number;
  total_additions: number;
  total_deletions: number;
  truncated: boolean;
}

export interface ToastMessage {
  id: number;
  message: string;
}

/** One configured lead model, as returned by the native runtime. */
export interface LeadModel {
  endpoint: string;
  model: string;
  /** The explicit `endpoint/model` spec the engine runs. */
  spec: string;
  is_default: boolean;
  /** Reasoning efforts this model accepts; empty = it takes none. */
  reasoning_efforts?: string[];
  /** The effort it runs at when none is chosen. */
  default_effort?: string | null;
}

/** The UI's answer to a native `ask_user` question. */
export type LeadAnswerInput =
  | { kind: "choice"; index: number }
  | { kind: "freeform"; text: string };

/** A pending interaction reported by the headless session snapshot. */
export interface LeadPendingInteraction {
  kind: "approval" | "question" | "host_tool_call" | "policy_hook" | (string & {});
  request_id?: string;
  question_id?: string;
  call_id?: string;
  hook_id?: string;
  tool_call_id?: string;
  tool_name?: string;
  question?: string;
  options?: { label: string; description?: string | null }[];
  [key: string]: unknown;
}

export interface LeadTranscriptToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface LeadTranscriptMessage {
  role: string;
  content: unknown;
  tool_calls?: LeadTranscriptToolCall[];
  tool_call_id?: string | null;
  name?: string | null;
  /** Whether the tool message reports a failed call (absent on old history). */
  tool_is_error?: boolean;
}

export interface LeadTranscript {
  messages: LeadTranscriptMessage[];
}

export interface LeadPlanStep {
  index: number;
  text: string;
  header?: string | null;
  done: boolean;
}

/** The active plan from this lead session's own plans directory. */
export interface LeadPlan {
  id: string;
  title: string;
  goal: string;
  steps: LeadPlanStep[];
}

/** The lead state returned by `lead_open`, `lead_snapshot`, and `lead_set_model`. */
export interface LeadSnapshot {
  generation: number;
  model: string | null;
  model_id: string | null;
  reasoning_effort?: string | null;
  models: LeadModel[];
  default_model: string | null;
  transcript: LeadTranscript;
  plan?: LeadPlan | null;
  used_tokens: number;
  max_tokens: number;
  pending: LeadPendingInteraction | null;
  busy: boolean;
}

/** The serialized inner agent event. */
export interface LeadAgentEvent {
  kind: string;
  text?: string;
  model?: string;
  message?: string;
  error?: string;
  retrying?: boolean;
  tool_name?: string;
  tool_call_id?: string;
  call_id?: string;
  id?: string;
  name?: string;
  request_id?: string;
  index?: number;
  fragment?: string;
  args?: unknown;
  stream?: string;
  result?: { success?: boolean; output?: unknown; data?: unknown; [key: string]: unknown };
  question_id?: string;
  question?: string;
  options?: { label: string; description?: string | null }[];
  [key: string]: unknown;
}

export interface LeadSessionEvent {
  agent: number;
  event: LeadAgentEvent;
  rates?: unknown;
}

/** One event envelope emitted on `mnemo2://lead-event`. */
export interface LeadEvent {
  generation: number;
  event: LeadSessionEvent;
}

export type LeadConnectionMode = "mock" | "native";
export type LeadConnectionStatus = "idle" | "opening" | "ready" | "error";

/** Frontend state for the real lead connection. */
export interface LeadRuntimeInfo {
  mode: LeadConnectionMode;
  status: LeadConnectionStatus;
  generation: number;
  busy: boolean;
  error: string | null;
  /** Whether the provider is automatically retrying the current error. */
  errorRetrying: boolean;
  model: string | null;
  modelId: string | null;
  /** The lead's chosen reasoning effort; null = the model's default. */
  reasoningEffort: string | null;
  models: LeadModel[];
  defaultModel: string | null;
  pending: LeadPendingInteraction | null;
  usedTokens: number;
  maxTokens: number;
  /** Per-role context breakdown from the last `context_usage` event; null when unknown. */
  contextBreakdown: { system: number; user: number; assistant: number; tool: number } | null;
  /** Whether the lead's context is currently being compacted. */
  compacting: boolean;
  /** Observed start of the active turn; null while idle or unknown on restore. */
  turnStartedAtMs: number | null;
  streamingEntryId: string | null;
}

export interface ProjectConflict {
  projectId: string;
  pid: number;
  startedAt: number | null;
}

/** Durable per-project usage aggregate behind the header cost metric. */
export interface ProjectUsage {
  /** Uncached input tokens (prompt tokens minus cached tokens). */
  inputTokens: number;
  /** Cached input tokens, counted separately from uncached input. */
  cachedTokens: number;
  /** Completion output tokens. */
  outputTokens: number;
  /** Estimated cost in integer micro-USD for priced usage only. */
  costMicros: number;
  /** Models with recorded usage but no configured or vendor price. */
  unpricedModels: string[];
}

/** One worker's row in the open deletion sheet. */
export interface WorkerDeletionRow {
  workerId: string;
  name: string;
  /** The worker's finished preview; null while its checks still run. */
  preview: WorkerDeletionPreview | null;
  /** Why this worker's deletion failed; null unless it failed. */
  error: string | null;
  /** The worker was deleted. */
  deleted: boolean;
}

/** An open worker-deletion sheet: rows stream in per worker. */
export interface WorkerDeletionRequest {
  /** "checking" while previews stream in, "ready" once every worker is checked, "deleting" while workers are removed. */
  phase: "checking" | "ready" | "deleting";
  rows: WorkerDeletionRow[];
}

export interface AppState {
  route: Route;
  projectId: string | null;
  projects: Project[];
  workers: Worker[];
  leadEntries: LeadEntry[];
  /** Unsent composer text and attachments for the open project. */
  leadDraft: LeadDraft;
  leadRuntime: LeadRuntimeInfo;
  selectedWorkerId: string | null;
  teamPeekOpen: boolean;
  teamFilter: TeamFilter;
  /** Real live-worker cap reported by the native team runtime. */
  teamSlots: number;
  /** Real count of live worker sessions. */
  teamActive: number;
  /** Durable per-project usage, including the lead and deleted workers. */
  projectUsage: ProjectUsage;
  workbenchOpen: boolean;
  workbenchTab: WorkbenchTab;
  workbenchWidth: number;
  /** Worker whose output streams in the lead-side panel; null when closed. */
  workerPanelId: string | null;
  browserUrl: string;
  browserTitle: string;
  browserLoading: boolean;
  browserSupported: boolean | null;
  browserError: string | null;
  browserImage: { dataUrl: string; label: string } | null;
  plan: LeadPlan | null;
  planItems: PlanItem[];
  /** Every plan on the board (open first), from the native host. */
  board: BoardPlan[];
  diff: DiffSnapshot | null;
  diffFiles: DiffFile[];
  diffWorkerId: string | null;
  diffLoading: boolean;
  diffError: string | null;
  projectConflict: ProjectConflict | null;
  /** The open project's startup code-index and memory passes. */
  indexing: IndexingSnapshot | null;
  /** Whether Claude Code leads the open project through the mnemo plugin. */
  externalLead: ExternalLeadState;
  cleanupProjectId: string | null;
  cleanupBusy: boolean;
  /** An open worker-deletion confirmation with what each delete removes. */
  workerDeletion: WorkerDeletionRequest | null;
  closeProjectPending: boolean;
  newProjectParent: string | null;
  creatingProject: boolean;
  toast: ToastMessage | null;
}
