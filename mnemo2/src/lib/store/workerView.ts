import { insertActivityCard, type ActivityCard } from "../teamActivity";
import {
  findingFromWire,
  inboxFromWire,
  reviewEvidenceFrom,
  reviewRiskFrom,
  waitsOnUser,
  workerDisplayName,
  workerKindFrom,
  type TeamMergeRecord,
  type TeamMessage,
  type TeamWorkerRecord,
} from "../teamState";
import type { Check, LeadEntry, MergeRecord, Worker, WorkerActivity, WorkerStatus } from "../types";
import { workerThreadFromRecord } from "../workerThread";
import { workerScreenshotPaths } from "../workbenchState";

export function timeFromMs(milliseconds: number): string {
  if (!Number.isFinite(milliseconds) || milliseconds <= 0) return "unknown";
  return new Date(milliseconds).toLocaleTimeString();
}

export function elapsedFromMs(milliseconds: number): string {
  if (!Number.isFinite(milliseconds) || milliseconds <= 0) return "—";
  const seconds = Math.floor(milliseconds / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

export function mergeFromRecord(worker: TeamWorkerRecord, merge: TeamMergeRecord): MergeRecord {
  return {
    id: `merge-${worker.id}-${merge.commit}`,
    workerId: worker.id,
    commit: merge.commit,
    branch: merge.branch,
    target: merge.target,
    files: merge.files,
    additions: merge.additions,
    deletions: merge.deletions,
    checks: merge.checks,
    reviewSummary: merge.review_summary,
    model: merge.worker_model ?? worker.model_id ?? worker.model ?? "",
    tokens: merge.worker_tokens,
    cost: merge.cost_micros == null ? null : merge.cost_micros / 1_000_000,
    time: timeFromMs(merge.merged_at_ms),
    revertCommit: merge.revert_commit ?? undefined,
  };
}

export function workerTimeline(record: TeamWorkerRecord): Worker["timeline"] {
  const time = (milliseconds: number) => milliseconds > 0 ? timeFromMs(milliseconds) : undefined;
  const timeline: Worker["timeline"] = [{
    id: `${record.id}-created`,
    label: "Task dispatched",
    detail: record.brief,
    status: "done",
    time: time(record.created_at_ms),
  }];
  if (record.turns > 0 || record.last_activity_ms > 0) {
    timeline.push({
      id: `${record.id}-activity`,
      label: "Worker activity",
      detail: `${record.turns} completed turn(s) · ${record.total_tokens} cumulative tokens`,
      status: ["working", "dispatching", "fixing"].includes(record.status) ? "active" : "done",
      time: time(record.last_activity_ms),
    });
  }
  for (const check of record.checks) {
    timeline.push({
      id: `${record.id}-check-${check.name}`,
      label: check.name,
      detail: check.detail || `Exact worker tip ${check.tip}`,
      status: check.status === "passed" ? "done" : check.status === "failed" ? "failed" : "active",
      time: time(record.last_activity_ms),
    });
  }
  if (record.review) {
    timeline.push({
      id: `${record.id}-review-${record.review.reviewed_at_ms}`,
      label: "Lead review recorded",
      detail: record.review.summary,
      status: record.findings.some((finding) => finding.status === "open") ? "active" : "done",
      time: time(record.review.reviewed_at_ms),
    });
  }
  if (record.merge) {
    timeline.push({
      id: `${record.id}-merge-${record.merge.commit}`,
      label: record.merge.revert_commit ? "Merge reverted" : `Landed in ${record.merge.target}`,
      detail: record.merge.revert_commit ?? record.merge.commit,
      status: "done",
      time: time(record.merge.merged_at_ms),
    });
  }
  if (record.status === "cancelled") {
    timeline.push({
      id: `${record.id}-cleaned`,
      label: "Checkout cleaned",
      detail: record.error ?? "The branch is retained for review.",
      status: "done",
    });
  } else if (["failed", "conflict", "needs_you"].includes(record.status)) {
    timeline.push({
      id: `${record.id}-attention`,
      label: "Needs attention",
      detail: record.error ?? "The worker stopped before finishing; open its thread for its last message.",
      status: "failed",
      time: time(record.last_activity_ms),
    });
  } else if (["stopped", "interrupted"].includes(record.status)) {
    timeline.push({
      id: `${record.id}-stopped`,
      label: "Session stopped",
      detail: "The branch and worktree are retained.",
      status: "done",
      time: time(record.last_activity_ms),
    });
  }
  return timeline;
}

/**
 * Map a team snapshot record to the worker the UI shows. The caller folds the
 * record's status into the live activity first (that map is store state, not a
 * pure function of the record) and passes it in.
 */
export function buildWorker(
  record: TeamWorkerRecord,
  index: number,
  messages: TeamMessage[],
  status: WorkerStatus,
  activity: WorkerActivity | null,
): Worker {
  const progress: Record<WorkerStatus, number> = {
    dispatching: 10,
    working: 45,
    needs_you: 45,
    idle: 50,
    paused: 45,
    interrupted: 45,
    completed: 70,
    stopped: 70,
    in_review: 85,
    fixing: 75,
    merging: 95,
    merged: 100,
    conflict: 85,
    failed: 70,
    cancelled: 0,
  };
  return {
    id: record.id,
    name: workerDisplayName(record.id, record.name),
    initials: workerDisplayName(record.id, record.name).replace(/[^A-Za-z0-9]/g, "").slice(0, 2).toUpperCase() || "W",
    hue: (index * 47 + 12) % 360,
    task: record.brief,
    summary: record.error ?? "",
    error: record.error ?? null,
    status,
    model: record.model_id ?? record.model ?? "",
    modelSpec: record.model,
    tier: record.tier ?? undefined,
    reasoningEffort: record.reasoning_effort ?? undefined,
    branch: record.branch,
    worktree: record.worktree,
    files: [],
    tokens: record.total_tokens,
    cost: record.cost_micros == null ? null : record.cost_micros / 1_000_000,
    elapsed: elapsedFromMs(record.elapsed_ms),
    turns: record.turns,
    compacting: record.compacting === true,
    progress: progress[status],
    lastActivity: timeFromMs(record.last_activity_ms),
    createdAtMs: record.created_at_ms,
    activity,
    needsYou:
      waitsOnUser(record.pending) || ["needs_you", "conflict", "failed"].includes(record.status),
    fixerFindingId: record.fixer_finding_id ?? undefined,
    kind: workerKindFrom(record.kind),
    reviewRisk: reviewRiskFrom(record.review_risk),
    auditTarget: record.audit_target ?? undefined,
    auditTip: record.audit_tip ?? undefined,
    reportDelivered: record.report_delivered === true,
    routing: record.routing ?? null,
    planId: record.plan_id ?? null,
    planStep: record.plan_step ?? null,
    checks: record.checks.map((check) => ({
      name: check.name,
      status:
        check.status === "passed" || check.status === "failed"
          ? (check.status as Check["status"])
          : "running",
      detail: check.detail ?? "",
      tip: check.tip,
    })),
    findings: record.findings.map(findingFromWire),
    review: record.review
      ? {
          summary: record.review.summary,
          tip: record.review.tip,
          reviewedAtMs: record.review.reviewed_at_ms,
          evidence: reviewEvidenceFrom(record.review.evidence),
        }
      : undefined,
    messages: workerThreadFromRecord(record, messages),
    inbox: (record.inbox ?? []).map(inboxFromWire),
    screenshots: workerScreenshotPaths(record.transcript),
    timeline: workerTimeline(record),
    diff: record.merge
      ? {
          files: record.merge.files,
          additions: record.merge.additions,
          deletions: record.merge.deletions,
        }
      : { files: 0, additions: 0, deletions: 0 },
    merge: record.merge ? mergeFromRecord(record, record.merge) : undefined,
  };
}

export function mergeTeamActivity(
  entries: LeadEntry[],
  messages: TeamMessage[],
  workers: TeamWorkerRecord[],
): LeadEntry[] {
  const next = entries.slice();
  // Each entry keeps the name it was recorded with, so it still names its
  // worker after that worker leaves the team list.
  const names = new Map(workers.map((worker) => [worker.id, workerDisplayName(worker.id, worker.name)]));
  const nameOf = (workerId: string, id: string) => {
    const known = names.get(workerId);
    if (known) return known;
    const previous = next.find((entry) => entry.id === id);
    return previous && "workerName" in previous && previous.workerName ? previous.workerName : workerId;
  };
  // A card already shown keeps its place; a new one goes where it happened,
  // never just to the bottom (that replayed every review after a restart).
  const fresh: ActivityCard[] = [];
  const upsert = (entry: ActivityCard) => {
    const index = next.findIndex((candidate) => candidate.id === entry.id);
    if (index === -1) fresh.push(entry);
    else next[index] = entry;
  };
  for (const message of messages) {
    const id = `team-${message.worker_id}-${message.at_ms}`;
    const attributed = next.some((entry) =>
      entry.id !== id && entry.kind === "team-message" &&
      entry.workerId === message.worker_id && message.text.trim().length > 0 &&
      entry.body.includes(message.text),
    );
    if (attributed) {
      const duplicate = next.findIndex((entry) => entry.id === id);
      if (duplicate !== -1) next.splice(duplicate, 1);
      continue;
    }
    upsert({
      id,
      kind: "team-message",
      workerId: message.worker_id,
      workerName: nameOf(message.worker_id, id),
      body: message.text,
      time: timeFromMs(message.at_ms),
      atMs: message.at_ms,
    });
  }
  for (const worker of workers) {
    if (worker.review) {
      const name = workerDisplayName(worker.id, worker.name);
      upsert({
        id: `review-${worker.id}-${worker.review.reviewed_at_ms}`,
        kind: "review",
        workerId: worker.id,
        workerName: name,
        title: `Reviewed ${name}’s work`,
        body: worker.review.summary,
        time: timeFromMs(worker.review.reviewed_at_ms),
        atMs: worker.review.reviewed_at_ms,
      });
    }
    if (worker.merge) {
      const record = mergeFromRecord(worker, worker.merge);
      upsert({
        id: record.id,
        kind: "merge",
        workerId: worker.id,
        workerName: workerDisplayName(worker.id, worker.name),
        record,
        time: record.time,
        atMs: worker.merge.merged_at_ms,
      });
    }
  }
  fresh
    .sort((left, right) => (left.atMs ?? 0) - (right.atMs ?? 0))
    .forEach((card) => insertActivityCard(next, card));
  return next;
}
