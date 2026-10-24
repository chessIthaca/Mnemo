export type Route = "start" | "lead" | "team";
export type WorkbenchTab = "browser" | "diff" | "plan";
export type TeamFilter = "all" | "needs" | "active" | "done";

export type WorkerStatus =
  | "dispatching"
  | "working"
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

export interface Project {
  id: string;
  name: string;
  path: string;
  branch: string;
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

export interface AppState {
  route: Route;
  projectId: string | null;
  projects: Project[];
  workers: Worker[];
  leadEntries: LeadEntry[];
  leadTimeline: TimelineStep[];
  activityExpanded: boolean;
  selectedWorkerId: string | null;
  teamPeekOpen: boolean;
  teamFilter: TeamFilter;
  workbenchOpen: boolean;
  workbenchTab: WorkbenchTab;
  workbenchWidth: number;
  browserUrl: string;
  planItems: PlanItem[];
  diffFiles: DiffFile[];
  toast: ToastMessage | null;
}
