// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { SplashProgress } from "./SplashCard";

/**
 * The reconcile phase BootSplash renders when the startup derived-index
 * reconciliation runs during the boot window (backlog 486955d5): the same
 * shape App's `reconcile` state tracks from the `memory://reconcile`
 * stream (`onReconcileEvent`).
 */
export type BootReconcile =
  | { phase: "running"; done: number; total: number }
  | { phase: "done"; summary: string }
  | { phase: "failed"; error: string };

/**
 * The full-screen boot splash (backlog 486955d5): shown while App's startup
 * checks (`getNeedsProject` / `getStartupSnapshot`) wait on the backend —
 * `build_brain` blocks the setup hook until `IpcState` is managed, so the
 * pre-index boot phases (config load, project open, memory store init)
 * would otherwise render App's empty main-UI shell.
 *
 * The visual is the React twin of the static pre-React splash in
 * `frontend/index.html` (same layout, colors, and text wordmark — not
 * MnemoLogo, which the static splash cannot use) so the static → React
 * handoff is seamless: the window goes from the static splash to this
 * without a visual jump, then to the picker / main UI / IndexingOverlay
 * once the checks resolve.
 *
 * When the startup memory-index reconciliation runs (a drifted memory DB:
 * a git merge landed, records were edited, or memory.db was deleted) it
 * can take seconds — the one open/create phase that can outlive ~1s with
 * no live feedback. App passes its tracked `reconcile` state down and the
 * splash renders "Building memory index" with the shared SplashProgress
 * bar (the same component the IndexingOverlay and the post-boot reconcile
 * dialog use). The reconcile is spawned inside `build_brain` before it
 * returns (a background task), and the startup checks cannot resolve
 * before `build_brain` returns — so the boot window always covers the
 * reconcile's start; a reconcile that outlives it hands off to the
 * post-boot dialog (running bar → running bar), which is why live
 * listening needs no snapshot. The done/failed texts are transient
 * hints: the post-boot reconcile dialog still carries the summary /
 * error until dismissed.
 */
export function BootSplash({
  reconcile = null,
}: {
  reconcile?: BootReconcile | null;
}) {
  return (
    <div
      className="flex h-screen w-screen flex-col items-center justify-center gap-3 bg-bg-primary"
      aria-label="Starting Mnemo"
    >
      <div className="text-2xl font-semibold tracking-widest text-slate-200">
        mnemo
      </div>
      {reconcile?.phase === "running" ? (
        <div className="w-64">
          <div className="mb-2 text-sm text-slate-400">Building memory index</div>
          <SplashProgress
            label="Reconciling memories…"
            right={
              reconcile.total > 0
                ? `${reconcile.done}/${reconcile.total}`
                : "…"
            }
            pct={
              reconcile.total > 0
                ? Math.round((reconcile.done / reconcile.total) * 100)
                : null
            }
          />
        </div>
      ) : (
        <>
          <div className="h-5 w-5 animate-spin rounded-full border-2 border-slate-700 border-t-cyan-400" />
          <div className="text-sm text-slate-400">
            {reconcile === null
              ? "Starting…"
              : reconcile.phase === "done"
                ? "Memory index ready"
                : "Memory index check failed"}
          </div>
        </>
      )}
    </div>
  );
}
