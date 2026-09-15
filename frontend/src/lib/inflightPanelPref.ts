// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Persisted open/closed preference for the InflightBar's reasoning/activity
// panel.
//
// Whether the panel is open is the USER's choice alone — no code path opens
// it on its own (user report 2027-01-11: the panel used to auto-expand
// whenever a reasoning block started streaming, so every turn re-opened a
// panel the user had deliberately collapsed). The choice is remembered across
// restarts and defaults to CLOSED for a fresh install.
//
// Guarded like the other persisted UI prefs (see `readLs`/`writeLs` in
// `../hooks/appearance.ts`): a missing `window` (vitest's node environment)
// and a throwing localStorage (quota / privacy mode) both degrade to the
// default instead of breaking the bar.

/** localStorage key persisting whether the reasoning panel is open. */
export const LS_INFLIGHT_PANEL_OPEN = "mh.inflightPanelOpen";

/**
 * Parse the persisted flag. Only the literal `"1"` means open — anything else
 * (absent, `""`, `"0"`, `"true"`, garbage) is closed, so a fresh install and a
 * corrupt value both land on the CLOSED default.
 */
export function parsePanelOpen(raw: string | null): boolean {
  return raw === "1";
}

/** Read the persisted panel state — CLOSED when absent, unreadable, or unavailable. */
export function readInflightPanelOpen(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return parsePanelOpen(window.localStorage.getItem(LS_INFLIGHT_PANEL_OPEN));
  } catch {
    return false;
  }
}

/** Persist the panel state (best-effort — never throws; `"1"` = open, `"0"` = closed). */
export function writeInflightPanelOpen(open: boolean): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(LS_INFLIGHT_PANEL_OPEN, open ? "1" : "0");
  } catch {
    // ignore quota / privacy-mode errors
  }
}
