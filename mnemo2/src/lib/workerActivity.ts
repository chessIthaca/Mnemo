import { toolSummary } from "./toolSummary";
import type { TeamEvent } from "./teamState";
import type { WorkerActivity, WorkerPhase, WorkerStatus } from "./types";

/**
 * Move the activity record into `phase`, keeping the turn anchor and the phase
 * clock: a repeated event returns the same object (no churn for the store), a
 * changed phase restarts the clock, and `started` re-anchors the turn.
 */
function moveTo(
  activity: WorkerActivity | null,
  phase: WorkerPhase,
  nowMs: number,
  overrides: { toolName?: string | null; turnStartedAtMs?: number | null } = {},
): WorkerActivity {
  const toolName =
    overrides.toolName !== undefined
      ? overrides.toolName
      : phase === "tool"
        ? (activity?.toolName ?? null)
        : null;
  const turnStartedAtMs =
    overrides.turnStartedAtMs !== undefined ? overrides.turnStartedAtMs : (activity?.turnStartedAtMs ?? null);
  const phaseSinceMs = activity?.phase === phase ? activity.phaseSinceMs : nowMs;
  if (
    activity !== null &&
    activity.phase === phase &&
    activity.toolName === toolName &&
    activity.turnStartedAtMs === turnStartedAtMs
  ) {
    return activity;
  }
  return { phase, toolName, turnStartedAtMs, phaseSinceMs };
}

/**
 * Fold one streamed worker event into its live activity record. Pure: the same
 * activity plus the same event plus the same clock always yields the same next
 * record, and an event that changes nothing returns the record unchanged.
 *
 * `started` re-anchors the turn timer; `finished` and `exited` clear the
 * activity — the worker is no longer running. Unknown event kinds (usage,
 * context, compaction…) leave the record alone.
 */
export function applyWorkerEvent(
  activity: WorkerActivity | null,
  event: TeamEvent,
  nowMs: number,
): WorkerActivity | null {
  const payload = event.event?.event as { kind?: string; name?: unknown } | undefined;
  switch (payload?.kind) {
    case "started":
      return moveTo(activity, "starting", nowMs, { toolName: null, turnStartedAtMs: nowMs });
    case "reasoning_delta":
      return moveTo(activity, "thinking", nowMs);
    case "text_delta":
      return moveTo(activity, "writing", nowMs);
    case "tool_call_start":
      return moveTo(activity, "tool", nowMs, {
        toolName: typeof payload.name === "string" && payload.name ? payload.name : null,
      });
    case "tool_call_arg_delta":
    case "tool_execution_start":
    case "tool_output_delta":
      // All three stream while one tool runs — hold the tool phase.
      return moveTo(activity, "tool", nowMs);
    case "tool_result":
      return moveTo(activity, "thinking", nowMs);
    case "approval_request":
    case "user_question":
      return moveTo(activity, "waiting", nowMs);
    case "finished":
    case "exited":
      return null;
    default:
      return activity;
  }
}

/**
 * Fold one status snapshot into the live activity record. Statuses arrive with
 * every team sync, so repeated snapshots must not reset the clock: a phase the
 * record already shows is kept as-is.
 *
 * `dispatching` means nothing has streamed yet — `starting`; `working` and
 * `fixing` leave the streamed phases alone (with nothing streamed yet the turn
 * is starting); `needs_you` waits on the human; every other status means the
 * worker is not running, so the activity clears.
 */
export function applyWorkerStatus(
  activity: WorkerActivity | null,
  status: WorkerStatus,
  nowMs: number,
): WorkerActivity | null {
  switch (status) {
    case "dispatching":
      if (activity?.phase === "starting") return activity;
      return { phase: "starting", toolName: null, turnStartedAtMs: null, phaseSinceMs: nowMs };
    case "working":
    case "fixing":
    case "merging":
      // The answer arrived and the turn continues: waiting hands back to thinking.
      if (activity?.phase === "waiting") return moveTo(activity, "thinking", nowMs);
      return activity ?? { phase: "starting", toolName: null, turnStartedAtMs: null, phaseSinceMs: nowMs };
    case "needs_you":
      return moveTo(activity, "waiting", nowMs);
    default:
      return null;
  }
}

/** The label for the `tool` phase: the team tools' phrase when one exists, the raw name otherwise. */
function toolPhaseLabel(toolName: string | null): string {
  if (!toolName) return "Running a tool…";
  const summary = toolSummary(toolName, "");
  // toolSummary's fallback is "Using <pretty name>" — the raw name reads friendlier there.
  return summary.startsWith("Using ") ? `Running ${toolName}…` : `${summary}…`;
}

/** The live-status label for one worker activity, e.g. "Thinking…" or "Running shell…". */
export function workerPhaseLabel(activity: WorkerActivity): string {
  switch (activity.phase) {
    case "starting":
      return "Starting…";
    case "thinking":
      return "Thinking…";
    case "writing":
      return "Writing…";
    case "tool":
      return toolPhaseLabel(activity.toolName);
    case "waiting":
      return "Waiting on you";
  }
}