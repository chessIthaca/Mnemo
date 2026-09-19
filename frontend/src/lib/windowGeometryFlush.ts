// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Flush handle for the persisted window geometry.
 *
 * The geometry save in App.tsx is debounced (400 ms after the last
 * move/resize), and a project switch restarts the process with a hard
 * `exit(0)` — no `beforeunload` ever runs. Without an explicit flush, a
 * move/resize made within the debounce window is lost across the restart
 * and the relaunched window opens at the previous bounds.
 *
 * App's geometry effect registers a flusher here (cancel the pending
 * debounce + save immediately); the project picker awaits
 * {@link flushWindowGeometry} right before `switchProject`.
 */

/** The registered flusher, or null while App's geometry effect is down. */
let flusher: (() => Promise<void>) | null = null;

/**
 * Register (or clear, with null) the current geometry flusher. Called by
 * App's window-geometry effect on mount and cleanup.
 */
export function registerWindowGeometryFlusher(
  fn: (() => Promise<void>) | null,
): void {
  flusher = fn;
}

/**
 * Flush the window-geometry save now: cancels the pending debounce and
 * persists the current bounds. A no-op when no flusher is registered (App
 * not mounted / the geometry effect torn down).
 */
export async function flushWindowGeometry(): Promise<void> {
  await flusher?.();
}
