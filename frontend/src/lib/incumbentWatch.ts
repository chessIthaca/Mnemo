// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Live liveness watch for the same-project instance-conflict dialog.
 *
 * The startup snapshot's `instanceConflict` is a launch-time snapshot: the
 * incumbent it names may exit while the dialog is on screen. While the
 * conflict is non-null App.tsx re-checks the incumbent's pid — on an
 * interval and on window focus — and retracts the dialog as soon as the
 * pid is dead, so closing the warned-about instance also clears the
 * surviving instance's warning without a restart (backlog ea9576d7; SPEC
 * 2027-01-11-same-project-instance-warning-is-a-launch-time-s.md).
 *
 * The probe is injectable so the unit tests can drive it without the
 * Tauri IPC bridge — `isIncumbentAlive` (lib/tauri.ts) is the production
 * probe. A rejected probe is NOT evidence of death: the watch keeps
 * running and only a definitive `false` retracts. pid recycling stays a
 * documented benign residual (a recycled pid keeps the dialog up).
 */

/** The liveness probe injected into [`watchIncumbentLiveness`]. */
export type IncumbentLivenessProbe = (pid: number) => Promise<boolean>;

/** How often the incumbent is re-checked while the dialog is visible. */
export const INCUMBENT_RECHECK_INTERVAL_MS = 2000;

/**
 * Watch the incumbent's liveness until it dies or the returned stop
 * function is called. Calls `onDead` exactly once — right after the probe
 * first reports the pid dead — and then stops itself.
 *
 * Returns the stop function (clears the interval + removes the focus
 * listener); stopping is permanent and idempotent. Probes never overlap —
 * an in-flight check suppresses the next tick.
 */
export function watchIncumbentLiveness(
  pid: number,
  isAlive: IncumbentLivenessProbe,
  onDead: () => void,
  intervalMs: number = INCUMBENT_RECHECK_INTERVAL_MS
): () => void {
  let stopped = false;
  let probeInFlight = false;
  let timer: ReturnType<typeof setInterval> | null = null;
  // The vitest environment is node (no DOM): the focus re-check is a
  // snappiness extra, so it degrades to interval-only there.
  const focusTarget: Window | null = typeof window === "undefined" ? null : window;
  const focusCheck = () => void check();

  async function check(): Promise<void> {
    if (stopped || probeInFlight) return;
    probeInFlight = true;
    try {
      // A rejected probe is unknown liveness, not death — keep watching.
      const alive = await isAlive(pid).catch(() => null);
      if (!stopped && alive === false) {
        stop();
        onDead();
      }
    } finally {
      probeInFlight = false;
    }
  }

  function stop(): void {
    if (stopped) return;
    stopped = true;
    if (timer !== null) {
      clearInterval(timer);
      timer = null;
    }
    focusTarget?.removeEventListener("focus", focusCheck);
  }

  timer = setInterval(() => void check(), intervalMs);
  focusTarget?.addEventListener("focus", focusCheck);
  return stop;
}