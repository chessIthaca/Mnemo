import type { WorkerRouting, WorkerStatus } from "./types";

/**
 * The mission-control routing chips (plan 2ba2168a, design §4.10): what the
 * dispatch-time decision did for one worker, and the one nudge it got. Pure —
 * the card renders, the tests pin the wording.
 */

/** A worker status that means the run is over, so its nudge chip clears. */
const FINISHED: readonly WorkerStatus[] = ["completed", "merged", "failed", "stopped"];

/**
 * The routing chip's text, or `null` when the dispatch recorded no decision.
 *
 * - a cleared answer reads `Laya: average 0.62` (the tier and its confidence);
 * - a decision Laya did not clear reads `lead chose difficult`;
 * - a dispatch with no tier at all reads `Laya unsure`.
 */
export function routingChipText(routing: WorkerRouting | null | undefined): string | null {
  if (!routing) return null;
  if (routing.cleared && routing.laya_tier !== null) {
    const confidence = routing.confidence === null ? "" : ` ${routing.confidence.toFixed(2)}`;
    return `Laya: ${routing.laya_tier}${confidence}`;
  }
  const chosen = routing.lead_tier ?? routing.ran_tier;
  return chosen === null ? "Laya unsure" : `lead chose ${chosen}`;
}

/**
 * The nudge chip's text, or `null` when no nudge is live. A nudge clears once
 * the worker runs on the tier the nudge suggested (`currentTier`) or the run
 * finished — it is a suggestion, never a switch.
 */
export function nudgeChipText(
  routing: WorkerRouting | null | undefined,
  currentTier: string | undefined,
  status: WorkerStatus | undefined,
): string | null {
  if (!routing || routing.nudged === null) return null;
  if (status !== undefined && FINISHED.includes(status)) return null;
  if (currentTier === routing.nudged) return null;
  return `Laya suggests ${routing.nudged} — stuck`;
}
