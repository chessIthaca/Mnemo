// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Display-layer filter for the search/search_read AUTO-DELEGATION note.
 *
 * When a symbol-shaped query is auto-delegated to the code graph or memory,
 * the tool returns a steering header — `AUTO-DELEGATED to the code graph —
 * 'X' is an indexed symbol (re-issue this exact search to get the plain
 * file search instead)` (memory twin: `AUTO-DELEGATED to memory — …`) —
 * above the delegated answer (def:/callers:/full 360° lines). Two emission
 * shapes: the FAST path (memory hunt, or symbol hunt with no glob) returns
 * the block raw; the PREPEND path (symbol hunt narrowed by a glob, riding
 * above normal results) wraps it as `note: AUTO-DELEGATED …`. That header
 * is MODEL guidance (the re-issue escape hatch works server-side
 * regardless of display), so the Chat setting
 * `[ui].show_delegation_notes` (default off) hides it from the ToolCard's
 * rendering only: the tool result text — the model's context — is never
 * modified, and the delegated answer always stays visible.
 *
 * React-free so the node-env unit tests can exercise it directly.
 */

/** Steering-header line prefixes, one per emission shape: the FAST path
 *  (memory hunt, or symbol hunt with no glob) returns the block raw — the
 *  header starts with `AUTO-DELEGATED`; the PREPEND path (symbol hunt
 *  narrowed by a glob, riding above normal results) wraps it in
 *  `with_note` — `note: AUTO-DELEGATED`. */
const NOTE_PREFIXES = ["note: AUTO-DELEGATED", "AUTO-DELEGATED"];

/**
 * Whether a search-result note (as extracted by `searchResultInfo` — the
 * note text WITHOUT the `note: ` prefix) is an auto-delegation note.
 */
export function isDelegationNote(note: string): boolean {
  return note.startsWith("AUTO-DELEGATED");
}

/**
 * Remove the AUTO-DELEGATED steering header line(s) from a tool result —
 * both emission shapes (raw fast-path block and `note: `-prefixed
 * prepend). Only the header line goes — the delegated answer
 * (def:/callers:/full 360° lines, memory hit lines) and every other note
 * (e.g. the content-index staleness note) stay.
 */
export function stripDelegationNotes(text: string): string {
  return text
    .split("\n")
    .filter((line) => !NOTE_PREFIXES.some((p) => line.startsWith(p)))
    .join("\n");
}
