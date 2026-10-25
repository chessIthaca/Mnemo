import type {
  TaskWorkflow,
  TaskWorkflowState,
  TaskWorkflowStep,
  TaskWorkflowStepId,
  TaskWorkflowStepStatus,
  Worker,
} from "./types";

const STEP_ORDER: TaskWorkflowStepId[] = ["brief", "implement", "verify", "review", "merge"];

const STEP_LABELS: Record<TaskWorkflowStepId, string> = {
  brief: "Brief",
  implement: "Implement",
  verify: "Verify",
  review: "Review",
  merge: "Merge",
};

interface StepState {
  status: TaskWorkflowStepStatus;
  detail: string;
}

type StepMap = Record<TaskWorkflowStepId, StepState>;

function initialSteps(worker: Worker): StepMap {
  return {
    brief: { status: "done", detail: "Brief accepted" },
    implement: {
      status: "done",
      detail: worker.diff.files > 0 ? `${worker.diff.files} files · +${worker.diff.additions} −${worker.diff.deletions}` : "No diff yet",
    },
    verify: { status: "pending", detail: "Check scope waits for the branch tip" },
    review: { status: "pending", detail: "Lead review after checks" },
    merge: { status: "pending", detail: "Serialized after review" },
  };
}

function checksPassed(worker: Worker): boolean {
  return worker.checks.length > 0 && worker.checks.every((check) => check.status === "passed");
}

function checksRunning(worker: Worker): boolean {
  return worker.checks.some((check) => check.status === "running");
}

function checksFailed(worker: Worker): boolean {
  return worker.checks.some((check) => check.status === "failed");
}

function verifyState(worker: Worker): StepState {
  if (checksFailed(worker)) return { status: "blocked", detail: "Checks failed on the branch tip" };
  if (checksRunning(worker)) return { status: "active", detail: "Checks running on the branch tip" };
  if (checksPassed(worker)) return { status: "done", detail: `${worker.checks[0].name} passed` };
  return { status: "pending", detail: "Check scope waits for the implement tip" };
}

function reviewState(worker: Worker): StepState {
  const open = worker.findings.filter((finding) => finding.status === "open");
  if (!checksPassed(worker)) return { status: "pending", detail: "Review waits for checks" };
  if (!worker.review) return { status: "active", detail: "Delta review required on the current tip" };
  if (open.length > 0) return { status: "active", detail: `${open.length} finding${open.length === 1 ? "" : "s"} open` };
  return { status: "done", detail: "Review closed" };
}

function build(worker: Worker, steps: StepMap, current: TaskWorkflowStepId, state: TaskWorkflowState): TaskWorkflow {
  return {
    current,
    state,
    steps: STEP_ORDER.map<TaskWorkflowStep>((id) => ({
      id,
      label: STEP_LABELS[id],
      status: steps[id].status,
      detail: steps[id].detail,
    })),
  };
}

/**
 * Derive one task's workflow from that task alone. Never consult a global run
 * index: parallel tasks are allowed to be at different steps at the same time.
 */
export function deriveTaskWorkflow(worker: Worker): TaskWorkflow {
  const steps = initialSteps(worker);

  switch (worker.status) {
    case "dispatching":
      steps.brief = { status: "active", detail: "Waiting for a worker slot" };
      steps.implement = { status: "pending", detail: "Not started" };
      return build(worker, steps, "brief", "active");

    case "working":
      steps.implement = { status: "active", detail: "Implementing in the task worktree" };
      return build(worker, steps, "implement", "active");

    case "failed":
      steps.implement = { status: "blocked", detail: "Needs a lead decision" };
      return build(worker, steps, "implement", "blocked");

    case "cancelled":
      steps.implement = { status: "blocked", detail: "Cancelled by the lead" };
      return build(worker, steps, "implement", "cancelled");

    case "conflict":
      steps.verify = verifyState(worker);
      steps.review = reviewState(worker);
      steps.merge = { status: "blocked", detail: "Conflict — route the task back" };
      return build(worker, steps, "merge", "blocked");

    case "fixing": {
      steps.verify = verifyState(worker);
      steps.review = checksPassed(worker)
        ? { status: "active", detail: `Fixer: ${worker.findings.find((finding) => finding.id === worker.fixerFindingId)?.title ?? "finding in progress"}` }
        : { status: "pending", detail: "Re-review follows the fixer tip" };
      const current: TaskWorkflowStepId = checksRunning(worker) || checksFailed(worker) ? "verify" : "review";
      return build(worker, steps, current, "active");
    }

    case "in_review":
      steps.verify = verifyState(worker);
      steps.review = reviewState(worker);
      if (!checksPassed(worker)) {
        return build(worker, steps, "verify", checksFailed(worker) ? "blocked" : "active");
      }
      if (!worker.review || worker.findings.some((finding) => finding.status === "open")) {
        return build(worker, steps, "review", "active");
      }
      steps.merge = { status: "active", detail: "Ready to merge" };
      return build(worker, steps, "merge", "active");

    case "merging":
      steps.verify = verifyState(worker);
      steps.review = reviewState(worker);
      steps.merge = { status: "active", detail: "Serialized merge in progress" };
      return build(worker, steps, "merge", "active");

    case "merged":
      steps.verify = verifyState(worker);
      steps.review = { status: "done", detail: "Review closed" };
      steps.merge = { status: "done", detail: `Merged into ${worker.merge?.target ?? "the lead branch"}` };
      return build(worker, steps, "merge", "done");

    // Other live lifecycle states stay on the task's own implementation lane.
    case "needs_you":
      steps.implement = { status: "active", detail: "Waiting on a lead answer" };
      return build(worker, steps, "implement", "blocked");

    case "idle":
      steps.implement = { status: "active", detail: "Session live, waiting for a message" };
      return build(worker, steps, "implement", "active");

    case "paused":
      steps.implement = { status: "active", detail: "Paused by the lead" };
      return build(worker, steps, "implement", "blocked");

    case "interrupted":
      steps.implement = { status: "active", detail: "Turn interrupted" };
      return build(worker, steps, "implement", "blocked");

    case "completed":
      steps.implement = { status: "done", detail: "Worker turn finished" };
      steps.verify = verifyState(worker);
      steps.review = { status: "pending", detail: "Lead review required" };
      steps.merge = { status: "pending", detail: "Merge follows review" };
      return build(worker, steps, "verify", "active");

    case "stopped":
      steps.implement = { status: "blocked", detail: "Stopped by the lead; branch kept" };
      return build(worker, steps, "implement", "blocked");
  }
}
