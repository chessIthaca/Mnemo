export type Route = "start" | "lead" | "team";
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
  merged: "Merged",
  conflict: "Conflict",
  failed: "Failed",
  cancelled: "Cancelled",
};

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
  budgetSpent: number;
  budgetLimit: number;
}

export interface Check {
  name: string;
  status: "passed" | "running" | "failed";
  detail: string;
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

export interface WorkerMessage {
  id: string;
  from: "worker" | "lead";
  body: string;
  time: string;
}

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
  cost: number;
  time: string;
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
  branch: string;
  worktree: string;
  files: string[];
  tokens: number;
  cost: number;
  elapsed: string;
  turns: number;
  progress: number;
  lastActivity: string;
  needsYou: boolean;
  fixerFindingId?: string;
  checks: Check[];
  findings: Finding[];
  messages: WorkerMessage[];
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

export type LeadEntry =
  | {
      id: string;
      kind: "message";
      time: string;
      author: "lead" | "user";
      body: string;
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
      workerId: string;
      body: string;
    }
  | {
      id: string;
      kind: "review";
      time: string;
      workerId: string;
      title: string;
      body: string;
    }
  | {
      id: string;
      kind: "merge";
      time: string;
      workerId: string;
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
}

export interface DiffFile {
  path: string;
  additions: number;
  deletions: number;
  lines: DiffLine[];
}

export interface ToastMessage {
  id: number;
  message: string;
}

/** One configured lead model, as returned by the native runtime. */
export interface LeadModel {
  endpoint: string;
  model: string;
  spec: string;
  is_default: boolean;
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

/** The lead state returned by `lead_open`, `lead_snapshot`, and `lead_set_model`. */
export interface LeadSnapshot {
  generation: number;
  model: string | null;
  model_id: string | null;
  models: LeadModel[];
  default_model: string | null;
  transcript: LeadTranscript;
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
  model: string | null;
  modelId: string | null;
  models: LeadModel[];
  defaultModel: string | null;
  pending: LeadPendingInteraction | null;
  usedTokens: number;
  maxTokens: number;
  streamingEntryId: string | null;
}

export interface AppState {
  route: Route;
  projectId: string | null;
  projects: Project[];
  workers: Worker[];
  leadEntries: LeadEntry[];
  leadRuntime: LeadRuntimeInfo;
  selectedWorkerId: string | null;
  teamPeekOpen: boolean;
  teamFilter: TeamFilter;
  /** Real live-worker cap reported by the native team runtime. */
  teamSlots: number;
  /** Real count of live worker sessions. */
  teamActive: number;
  workbenchOpen: boolean;
  workbenchTab: WorkbenchTab;
  workbenchWidth: number;
  browserUrl: string;
  planItems: PlanItem[];
  diffFiles: DiffFile[];
  toast: ToastMessage | null;
}
