// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect } from "react";
import { browserWebviewOverlayEnter, browserWebviewOverlayExit } from "../lib/tauri";

/**
 * Hide the native child WebView2 while a full-viewport modal is open, then
 * show it again when the modal closes.
 *
 * The child WebView2 is a separate OS-level HWND composited ABOVE the app's
 * HTML. A full-viewport modal (Settings, About, the project picker, the merge
 * confirm, the safety toggle) would otherwise be punched through by the native
 * HWND — the modal's backdrop/content would render *behind* the child webview.
 * This hook tells the backend to hide the child on enter + show it on exit.
 *
 * The backend tracks an overlay *depth counter* (not a bool), so nested modals
 * balance correctly — and it's saturating, so a stray exit (e.g. React
 * strict-mode double-cleanup) can't underflow and wedge the webview hidden.
 *
 * No-op when `open` is false (the modal isn't shown). The enter call fires on
 * mount-while-open; the exit call fires on unmount-or-close.
 *
 * @param open whether the modal is currently open.
 */
export function useBrowserOverlay(open: boolean): void {
  useEffect(() => {
    if (!open) return;
    void browserWebviewOverlayEnter();
    return () => {
      void browserWebviewOverlayExit();
    };
  }, [open]);
}
