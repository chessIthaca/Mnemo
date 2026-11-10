// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The Trace tab's steering counters.
 *
 * A steering note rides inside a tool result to point the agent at a cheaper
 * or more direct tool. `trace_steering_stats` returns how often each note kind
 * fired this app run and how often the agent's very next call did what the
 * note asked; the counters sit on the engine's one loop factory, so one
 * snapshot covers the lead and every worker. The same snapshot counts file
 * edits by vehicle: the file tools versus the shell.
 *
 * The field names mirror `mnemo::SteeringStatsSnapshot` (serde keeps
 * snake_case), and the marker labels are keyed by the engine's stable kebab
 * labels (`marker()` in src/agent/steering_stats.rs).
 */

import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/** One note kind's counters (`MarkerCounts`). */
export interface SteeringMarker {
  /** The engine's stable kebab label, e.g. `search-nudge`. */
  marker: string;
  /** How many tool results carried this note. */
  fired: number;
  /** How often the next call named one of the note's target tools. */
  switched: number;
  /** How often the edit gate blocked an edit; only `edit-stale-read` has any. */
  gated: number;
}

/** This app run's steering counters (`SteeringStatsSnapshot`). */
export interface SteeringStats {
  /** Every note shown, all kinds together. */
  fired: number;
  /** Every note the agent's next call followed, all kinds together. */
  switched: number;
  /** The per-kind breakdown; every kind is present, even at zero. */
  by_marker: SteeringMarker[];
  /** Successful `file_edit`/`file_write`/`file_append` calls. */
  file_tool_mutations: number;
  /** File writes attempted through the shell (a heuristic, so it can overcount). */
  shell_mutations: number;
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/** The Tauri call the transport needs; injectable for tests. */
export interface SteeringTransportBindings {
  invoke: typeof invoke;
}

/** What the Insights panel needs from the host: one counters snapshot. */
export interface SteeringTransport {
  stats(): Promise<SteeringStats>;
}

/** The real host transport. */
export function createTauriSteeringTransport(
  bindings: SteeringTransportBindings,
): SteeringTransport {
  return {
    stats: () => bindings.invoke<SteeringStats>("trace_steering_stats"),
  };
}

/**
 * The steering transport, or `null` outside the desktop app: a plain browser
 * page has no Tauri IPC, so the section shows its quiet line instead.
 */
export function createSteeringTransport(
  bindings?: SteeringTransportBindings,
): SteeringTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriSteeringTransport(bindings ?? { invoke });
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

/**
 * Plain-language names for the engine's marker labels, in the order the notes
 * usually appear in a turn.
 */
export const MARKER_LABELS: Readonly<Record<string, string>> = {
  "search-nudge": "Use the code graph",
  "shell-tip": "A cheaper tool than the shell",
  "graph-miss": "Graph miss, try search",
  "recall-rider": "Recalled context",
  "read-nudge": "Outline for a big file",
  "literal-tip": "Use a literal search",
  "known-memory-hit": "Known memory",
  "consolidation-due": "Memory consolidation due",
  "shell-redirect": "Output sent to null",
  "edit-stale-read": "Re-read before editing",
};

/** A note kind's plain name; an unknown kind shows its own label. */
export function markerLabel(marker: string): string {
  return MARKER_LABELS[marker] ?? marker;
}

/** How often the notes were followed, as a whole percent (`—` when none fired). */
export function followRate(fired: number, switched: number): string {
  if (fired <= 0) return "—";
  return `${Math.round((switched / fired) * 100)}%`;
}

/** The kinds that fired this run, busiest first, then by label. */
export function steeringRows(stats: SteeringStats): SteeringMarker[] {
  return stats.by_marker
    .filter((row) => row.fired > 0)
    .sort((a, b) => b.fired - a.fired || a.marker.localeCompare(b.marker));
}

/** Whether the run has anything to report at all. */
export function hasSteeringData(stats: SteeringStats | null): boolean {
  if (stats === null) return false;
  return (
    stats.fired > 0 || stats.file_tool_mutations > 0 || stats.shell_mutations > 0
  );
}
