// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Pure state mapping for the Settings → Memory & Search maintenance tools:
 * folds a maintenance event (the `memory://maintenance` stream, or the
 * `codegraph://maintenance` rebuild stream) into the per-op UI state
 * (progress bar + result line). Extracted from the component so the event →
 * state contract is unit-testable without React/Tauri (mirrors the
 * agentEventReducer pattern).
 */
import type { MaintenanceOp } from "../../../lib/tauri";

/**
 * The shared event shape both maintenance streams fold from. The memory
 * stream's events carry an `op` routing field (ignored here — routing is
 * the caller's concern); the codegraph rebuild stream's events omit it.
 */
export type OpEvent =
  | { type: "started"; op?: MaintenanceOp }
  | {
      type: "progress";
      op?: MaintenanceOp;
      done: number;
      total: number;
      phase: string;
    }
  | { type: "done"; op?: MaintenanceOp; summary: string }
  | { type: "failed"; op?: MaintenanceOp; error: string };

/** The UI state for one maintenance operation (cleanup or rebuild). */
export interface OpState {
  /** Whether the op is currently running (bar visible). */
  running: boolean;
  /** Completed units from the latest progress tick. */
  done: number;
  /** Total units from the latest tick (`0` = label-only phase). */
  total: number;
  /** The latest phase label ("consolidating" / "embedding" / …). */
  phase: string;
  /** The terminal success summary (`null` until the op completes). */
  summary: string | null;
  /** The terminal failure text (`null` unless the op failed). */
  error: string | null;
}

/** The idle state both ops start (and remount) in. */
export function idleOp(): OpState {
  return {
    running: false,
    done: 0,
    total: 0,
    phase: "",
    summary: null,
    error: null,
  };
}

/**
 * Fold one maintenance event into an op's UI state. Accepts the shared
 * {@link OpEvent} shape: the memory stream's events (which carry an extra
 * `op` field) and the codegraph rebuild stream's events both fold here —
 * routing to the right op's state is the caller's concern.
 */
export function applyMaintenanceEvent(
  state: OpState,
  event: OpEvent,
): OpState {
  switch (event.type) {
    case "started":
      // A fresh start clears any previous result + stale progress.
      return { ...idleOp(), running: true };
    case "progress":
      return {
        ...state,
        running: true,
        done: event.done,
        total: event.total,
        phase: event.phase,
        summary: null,
        error: null,
      };
    case "done":
      return {
        ...state,
        running: false,
        phase: "",
        summary: event.summary,
        error: null,
      };
    case "failed":
      return {
        ...state,
        running: false,
        phase: "",
        summary: null,
        error: event.error,
      };
  }
}

/**
 * Subscribe with unmount-safe disposal (review B1): if disposal runs before
 * the subscribe promise resolves — e.g. the Settings dialog closes within
 * that window, or React StrictMode double-mounts in dev — the just-resolved
 * unlisten is invoked immediately, so no listener leaks on a dead component
 * (which would keep firing setState into an unmounted section forever).
 * Generic over the event type: both maintenance streams use it.
 */
export function subscribeUntilDisposed<E>(
  subscribe: (handler: (event: E) => void) => Promise<() => void>,
  handler: (event: E) => void,
): () => void {
  let disposed = false;
  let unlisten: (() => void) | null = null;
  void subscribe(handler).then((fn) => {
    if (disposed) fn();
    else unlisten = fn;
  });
  return () => {
    disposed = true;
    unlisten?.();
  };
}
