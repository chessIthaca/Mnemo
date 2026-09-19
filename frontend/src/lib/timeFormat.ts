// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Shared time formatters for the chat surfaces.
 *
 * `fmtTs` renders a wall-clock timestamp for display (the Conversation
 * hover-timestamp tooltip and the tool-card timing suffix): time-only for
 * today's entries, date+time otherwise. `fmtToolDuration` renders a tool
 * call's elapsed time compactly (412ms / 2.3s / 1m 12s).
 */

/**
 * Wall-clock timestamp: time-only for today's entries, date+time otherwise
 * (an old restored transcript shows its date too, so the time is never
 * ambiguous).
 */
export function fmtTs(ts: number): string {
  const d = new Date(ts);
  return d.toDateString() === new Date().toDateString()
    ? d.toLocaleTimeString()
    : d.toLocaleString();
}

/**
 * A tool call's elapsed time, compact: under a second in ms (`412ms`),
 * under a minute in one-decimal seconds (`2.3s`), else minutes + seconds
 * (`1m 12s`). The boundaries keep the common fast tool readable at a
 * glance while a long build stays honest about its length.
 */
export function fmtToolDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  let m = Math.floor(ms / 60_000);
  let s = Math.round((ms % 60_000) / 1000);
  // A rounded 60s carries into the minutes — 1m 59.999s must not render
  // as "1m 60s".
  if (s === 60) {
    m += 1;
    s = 0;
  }
  return `${m}m ${s}s`;
}
