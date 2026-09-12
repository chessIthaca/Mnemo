// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import type { TurnPhase } from "./types";
import type { ActivityEntry } from "../hooks/agentState";

/**
 * Whether a reasoning block is currently in view — the signal the
 * InflightBar's reasoning panel uses to auto-expand while thinking streams.
 *
 * True while the turn's live phase is `"reasoning"`, and stays true after the
 * phase moves on (e.g. `"streaming"`/answering) as long as the activity log
 * still holds a reasoning entry, so the panel remains open for the rest of
 * the block. A new turn clears the log (`started`), which drops the flag back
 * to false and arms the next block's auto-expand.
 */
export function reasoningActive(
  phase: TurnPhase,
  activityLog: ActivityEntry[],
): boolean {
  if (phase === "reasoning") return true;
  return activityLog.some((e) => e.kind === "reasoning");
}

/**
 * Whether a reasoning block just STARTED (inactive → active). The panel
 * auto-expands exactly once per block on this edge — a manual collapse
 * mid-block is respected (no re-expand while the flag stays true), and a
 * later collapse followed by the flag dropping false re-arms the next block.
 */
export function reasoningBlockStarted(
  wasActive: boolean,
  isActive: boolean,
): boolean {
  return isActive && !wasActive;
}
