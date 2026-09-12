// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Deep-link a file into the File viewer: reveal + select the right panel's
 * Files tab and record the path in the store (`pendingFileOpen`) for the
 * FileViewer to consume.
 *
 * Uses a store-held pending path — NOT a fire-and-forget CustomEvent — so it
 * is race-free: the FileViewer reads `pendingFileOpen` whenever it mounts,
 * even if the Files tab was disabled/unmounted at click time. (The previous
 * CustomEvent approach dropped the event whenever the viewer wasn't already
 * mounted — the default state, where the Files tab starts disabled.)
 *
 * Called from tool-card file-name links (file_read, file_write,
 * write_review_report, read_files, …) — `file_edit` chips instead deep-link
 * into the Diff tab via [`openDiffInViewer`].
 */
import { useAgentStore } from "../hooks/useAgentStore";

/**
 * Open `path` (project-relative, forward slashes) in the Files tab, optionally
 * scrolled to a 1-based `line` — a read_files link opens the file at the line
 * the read started (SourceEditor's revealLine does the scroll).
 */
export function openFileInViewer(path: string, line?: number | null): void {
  useAgentStore.getState().requestFileOpen(path, line ?? null);
}

/**
 * Deep-link a file's change into the right panel's Diff tab: reveal + select
 * the Diff tab and point `selectedDiffPath` at the file (DiffViewer renders
 * that file's entry). Called from `file_edit` tool-card file-name links —
 * every other tool keeps [`openFileInViewer`].
 */
export function openDiffInViewer(path: string): void {
  const store = useAgentStore.getState();
  store.revealRightPanelTab("diff");
  store.selectDiffPath(path);
}
