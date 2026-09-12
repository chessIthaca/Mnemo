// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useState } from "react";
import { RefreshCw, X } from "lucide-react";
import {
  getIndexProgress,
  onIndexProgress,
  type IndexProgressEvent,
} from "../../lib/tauri";
import { useBrowserOverlay } from "../../hooks/useBrowserOverlay";
import { SplashCard, SplashProgress } from "../common/SplashCard";

/**
 * The overlay's UI state, derived purely from the index-progress stream:
 * `null` = hidden (no pass running / finished cleanly), `progress` = visible
 * bar + counter, `failed` = visible error card with a Dismiss button.
 */
export type IndexingOverlayState =
  | null
  | { kind: "progress"; done: number; total: number }
  | { kind: "failed"; error: string };

/**
 * Fold one `IndexProgressEvent` into the overlay state (pure, testable —
 * mirrors the `applyMaintenanceEvent` pattern). `started`/`progress` make the
 * overlay visible; `done` hides it again (the app is ready); `failed` keeps
 * it visible with the error + a Dismiss button.
 */
export function applyIndexProgress(
  _state: IndexingOverlayState,
  event: IndexProgressEvent,
): IndexingOverlayState {
  switch (event.type) {
    case "started":
      return { kind: "progress", done: 0, total: 0 };
    case "progress":
      return { kind: "progress", done: event.done, total: event.total };
    case "done":
      // Terminal success — the overlay hides itself; the user did nothing
      // wrong and the summary ("N files indexed · …") would only delay them.
      return null;
    case "failed":
      return { kind: "failed", error: event.error };
  }
}

/**
 * How long the overlay stays visible at minimum once shown (ms). A fast
 * project switch would otherwise flash the dialog for a few frames between
 * `started` and `done`; holding it briefly makes the open read as
 * intentional (the same perceptual trick as skeleton screens).
 */
export const MIN_VISIBLE_MS = 800;

/**
 * How much longer the overlay should stay visible before hiding, given when
 * it was shown (`null` = not currently visible) and the current time. 0 when
 * the minimum window has already elapsed (or nothing is showing) — hide
 * immediately; otherwise the remaining milliseconds (pure, testable).
 */
export function hideDelayMs(shownAtMs: number | null, nowMs: number): number {
  if (shownAtMs === null) return 0;
  const visible = nowMs - shownAtMs;
  return visible >= MIN_VISIBLE_MS ? 0 : MIN_VISIBLE_MS - visible;
}

/**
 * The open-project indexing overlay. Self-subscribing and self-hiding: on
 * mount it listens on `codegraph://index-progress` (the stream the STARTUP
 * indexing pass and the create-project seed pass report on) and renders a
 * full-screen modal with a progress bar and a mono "N/M files indexed"
 * counter whenever a pass is running. Covers BOTH app start and project
 * switches: a switch restarts the process and the new launch's startup pass
 * emits `started` immediately (the 1s cold-start gate is skipped after a
 * switch), so the same splash dialog shows. Because the webview is
 * (re)created on every start/switch, the pass may have begun before this
 * component mounts — after subscribing it fetches `get_index_progress` once
 * and seeds its bar from the snapshot so early events are not lost.
 * Self-hiding: a terminal `done` clears the overlay, but never sooner than
 * {@link MIN_VISIBLE_MS} after it appeared (a pass that finishes in a few
 * frames is held briefly so the dialog doesn't flash); a lone `done` with
 * nothing on screen folds to null invisibly (cold starts of already-indexed
 * projects stay flash-free). Terminal failure keeps the overlay up with the
 * error text + a Dismiss button; the indexing itself is best-effort and the
 * app remains usable.
 *
 * Mounted in two places, exactly one of which is ever live: App.tsx's
 * normal tree (covers the startup pass and every switch-mode create flow —
 * the switch-mode picker only ever renders with a project open, so that
 * instance is already present; it must NOT mount a second copy inside its
 * Dialog, or the failed card would need two Dismiss clicks) and the
 * startup-mode ProjectPicker (covers the create-project first index when
 * the app started outside any project, where App's tree is not mounted).
 */
export function IndexingOverlay() {
  const [state, setState] = useState<IndexingOverlayState>(null);

  // The overlay is a full-viewport modal — hide the native child WebView2
  // while it is visible, like every other modal in the app (the child HWND
  // is composited above the HTML and would punch through the backdrop).
  // Called unconditionally (rules of hooks); a no-op while hidden.
  useBrowserOverlay(state !== null);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    // When the overlay first became visible (Date.now()), or null while
    // hidden — drives the minimum-visible hold on the terminal `done`.
    let shownAt: number | null = null;
    // The pending minimum-visible hide timer, if one is scheduled.
    let hideTimer: number | null = null;
    // Bumped on every event; a pending hide timer whose token is stale (a
    // newer event arrived mid-hold) must not fire its setState.
    let token = 0;

    /** Fold one event into state, honoring the minimum-visible hold. */
    function handleEvent(event: IndexProgressEvent) {
      const mine = ++token;
      if (hideTimer !== null) {
        window.clearTimeout(hideTimer);
        hideTimer = null;
      }
      if (event.type === "done") {
        const delay = hideDelayMs(shownAt, Date.now());
        if (delay > 0) {
          // Hold the bar briefly so a fast switch doesn't flash.
          hideTimer = window.setTimeout(() => {
            hideTimer = null;
            if (token !== mine) return; // a newer event took over
            shownAt = null;
            setState(null);
          }, delay);
          return;
        }
        shownAt = null;
        setState(null);
        return;
      }
      // started / progress / failed — visible again (or still visible).
      if (shownAt === null) shownAt = Date.now();
      setState((prev) => applyIndexProgress(prev, event));
    }

    void onIndexProgress(handleEvent).then((fn) => {
      // Disposal before the subscribe promise resolved (StrictMode
      // double-mount, quick unmount) → drop the just-resolved listener.
      if (disposed) {
        fn();
        return;
      }
      unlisten = fn;
      // Catch-up: the startup pass may have begun before this mount
      // subscribed (the webview is recreated on every start AND every
      // project switch, so early events are gone). Seed the bar from the
      // backend snapshot without clobbering an event that already arrived.
      // The token guard bails when ANY event arrived while the fetch was in
      // flight — events are always fresher than the snapshot, and a response
      // resolving after the terminal `done` was consumed must not reseed a
      // frozen overlay no future event will ever clear (H3, 2026-08-28
      // review).
      const myToken = token;
      void getIndexProgress()
        .then((snap) => {
          if (disposed || snap === null || token !== myToken) return;
          setState((prev) =>
            prev ?? { kind: "progress", done: snap.done, total: snap.total },
          );
          if (shownAt === null) shownAt = Date.now();
        })
        .catch(() => {
          /* best-effort catch-up — live events still drive the overlay */
        });
    });
    return () => {
      disposed = true;
      unlisten?.();
      if (hideTimer !== null) window.clearTimeout(hideTimer);
    };
  }, []);

  if (state === null) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm">
      <SplashCard>
        <div className="mb-2 flex items-center gap-2 text-sm font-semibold text-slate-200">
          <RefreshCw
            className={`h-4 w-4 text-cyan-400 ${
              state.kind === "progress" ? "animate-spin" : ""
            }`}
          />
          Indexing project
        </div>
        {state.kind === "progress" ? (
          <SplashProgress
            label="Indexing files…"
            right={
              state.total > 0
                ? `${state.done}/${state.total} files indexed`
                : "…"
            }
            pct={
              state.total > 0
                ? Math.round((state.done / state.total) * 100)
                : null
            }
          />
        ) : (
          <>
            <p className="mb-3 break-words text-xs text-red-400">
              Indexing failed: {state.error}
            </p>
            <button
              type="button"
              onClick={() => setState(null)}
              aria-label="Dismiss"
              className="w-full rounded-lg bg-cyan-600 px-4 py-1.5 text-sm font-medium text-white transition-colors hover:bg-cyan-500"
            >
              <X className="mr-1 inline h-3.5 w-3.5" />
              Dismiss
            </button>
          </>
        )}
      </SplashCard>
    </div>
  );
}