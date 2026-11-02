/**
 * The UI log: every popup and every uncaught frontend error, mirrored to disk.
 *
 * A popup can vanish before it is read — an info toast clears itself after
 * 4.2 s (`Toast.svelte`) — so a message the user (or the lead) needs to look up
 * afterwards must survive somewhere. This module is the frontend half: it
 * batches entries and hands them to the host's `ui_log` command
 * (`src-tauri/src/mnemo2/ui_log.rs`), which appends one JSON line per entry to
 * `<project>/.coding/logs/mnemo2-ui.jsonl` (or `<config dir>/logs/` when no
 * project is open).
 *
 * Logging is one-way and never blocks or breaks the page:
 * - entries are queued and flushed on a timer, so a burst (a team failing at
 *   once) costs one IPC round-trip and no frame;
 * - a browser-only Vite session has no host to write to and drops its queue;
 * - a rejected `ui_log` is swallowed — a broken log must never become a new
 *   error the user has to deal with.
 *
 * [`installUiErrorLogging`] wires the two uncaught paths (`window`'s `error`
 * event, which is what `window.onerror` reports, and `unhandledrejection`) to
 * the same log with level `error`, and deliberately shows no popup.
 */

import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";
import type { ToastLevel } from "./types";

/** How loud one logged entry is; mirrors a toast's level. */
export type UiLogLevel = ToastLevel;

/** One line of the UI log, exactly as the host writes it. */
export interface UiLogEntry {
  /** When the entry happened, in Unix epoch milliseconds. */
  ts: number;
  level: UiLogLevel;
  /** Where it came from: `toast`, `window` or `promise`. */
  source: string;
  message: string;
  /** The screen the app was on, so a vanished popup can be placed. */
  route?: string;
  worker_id?: string;
  /** Extra context: an exception's stack, the failing command's detail. */
  detail?: string;
}

/** Optional context a caller attaches to one entry. */
export interface UiLogExtra {
  source?: string;
  route?: string;
  worker_id?: string;
  detail?: string;
}

/** How long a burst is held before it is sent, so one IPC carries all of it. */
const FLUSH_DELAY_MS = 250;

/** A queue this long is sent at once instead of waiting for the timer. */
const MAX_PENDING = 50;

let pending: UiLogEntry[] = [];
let timer: ReturnType<typeof setTimeout> | undefined;

/** One entry with only the optional fields the caller actually filled in. */
function buildEntry(level: UiLogLevel, message: string, extra: UiLogExtra): UiLogEntry {
  const entry: UiLogEntry = {
    ts: Date.now(),
    level: level === "error" ? "error" : "info",
    source: extra.source?.trim() || "ui",
    message,
  };
  if (extra.route) entry.route = extra.route;
  if (extra.worker_id) entry.worker_id = extra.worker_id;
  if (extra.detail) entry.detail = extra.detail;
  return entry;
}

/**
 * Queue one popup or error for the UI log.
 *
 * Never throws and never waits: the entry is appended to the next batch, which
 * a timer sends in the background.
 */
export function logUi(level: UiLogLevel, message: string, extra: UiLogExtra = {}): void {
  try {
    pending.push(buildEntry(level, message, extra));
    if (pending.length >= MAX_PENDING) {
      void flushUiLog();
      return;
    }
    if (timer === undefined) {
      timer = setTimeout(() => {
        timer = undefined;
        void flushUiLog();
      }, FLUSH_DELAY_MS);
    }
  } catch {
    // Logging must never break the UI it is logging about.
  }
}

/**
 * Send every queued entry now (the timer path calls this on its own).
 *
 * Resolves once the host has answered; a missing host or a rejected command is
 * swallowed, so the caller never has to handle a logging failure.
 */
export async function flushUiLog(): Promise<void> {
  if (timer !== undefined) {
    clearTimeout(timer);
    timer = undefined;
  }
  if (pending.length === 0) return;
  const entries = pending;
  pending = [];
  try {
    // A browser-only Vite session has no host to write to.
    if (!isTauriRuntime()) return;
    await invoke("ui_log", { entries });
  } catch {
    // A failed write is the host's business (it notes it on stderr); the page
    // carries on.
  }
}

/** The string an uncaught value should be logged under. */
function describeThrown(value: unknown): string {
  if (value instanceof Error) return value.message || value.name || "Error";
  if (typeof value === "string") return value || "Empty error";
  if (value === null || value === undefined) return "Unknown error";
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

/** A thrown value's stack, when it carries one. */
function stackOf(value: unknown): string | undefined {
  return value instanceof Error && typeof value.stack === "string" && value.stack !== ""
    ? value.stack
    : undefined;
}

/**
 * Log uncaught frontend errors and unhandled rejections, without a popup.
 *
 * Nothing is shown to the user on purpose: an exception is already visible in
 * the devtools, and a toast for every stray rejection would bury the real ones.
 * The log keeps the message and stack so a failure that emptied the screen can
 * still be looked up afterwards.
 */
export function installUiErrorLogging(target: Window = window): void {
  target.addEventListener("error", (event) => {
    const failure = event as ErrorEvent;
    logUi("error", failure.message || describeThrown(failure.error), {
      source: "window",
      route: locationRoute(),
      detail: stackOf(failure.error),
    });
  });
  target.addEventListener("unhandledrejection", (event) => {
    const rejection = event as PromiseRejectionEvent;
    logUi("error", describeThrown(rejection.reason), {
      source: "promise",
      route: locationRoute(),
      detail: stackOf(rejection.reason),
    });
  });
}

/** The current screen, when the page is mounted; `undefined` in tests. */
function locationRoute(): string | undefined {
  try {
    return typeof location !== "undefined" && location.hash !== ""
      ? location.hash.replace(/^#\/?/, "")
      : undefined;
  } catch {
    return undefined;
  }
}
