// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The project picker's busy phases (backlog 486955d5): which long-running
 * action the picker is in the middle of, surfaced on the action buttons so
 * the user sees WHAT is happening instead of a bare "Working…". The
 * codegraph seed/index phase of a create is covered separately by the
 * IndexingOverlay; these labels cover the rest (scaffolding, registry
 * work, and the switch_project restart, which never returns — the process
 * relaunches into the new project).
 */
export type BusyPhase = "preparing" | "restarting" | "removing";

/**
 * The create button's label for the current phase — the create flow walks
 * preparing → restarting (createProject scaffolds + seeds, then
 * switchProject restarts); removing is the registry-remove flow sharing
 * the same busy gate.
 */
export function createButtonLabel(phase: BusyPhase | null): string {
  switch (phase) {
    case "preparing":
      return "Preparing project…";
    case "restarting":
      return "Restarting…";
    case "removing":
      return "Removing…";
    default:
      return "Create & open";
  }
}

/**
 * The per-row Open button's label — only the restart phase changes it
 * (open goes straight to switchProject, which restarts the process).
 */
export function openButtonLabel(phase: BusyPhase | null): string {
  return phase === "restarting" ? "Restarting…" : "Open";
}
