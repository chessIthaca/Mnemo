// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, type RefObject } from "react";
import { browserWebviewSetRect } from "../lib/tauri";

/**
 * Coalesce bursts of `call()`s into at most one `fn` execution per animation
 * frame. `raf`/`caf` are injected so tests can drive frames without a DOM.
 * While the window is occluded the frame never fires, so queued work is
 * silently deferred — exactly what a rect report wants (it only matters when
 * visible). R3 of .coding/reviews/2026-08-18-rdp-freeze-diagnosis.md.
 */
export function rafCoalesce(
  fn: () => void,
  raf: (cb: () => void) => number,
  caf: (id: number) => void,
): { call(): void; cancel(): void } {
  let pending = 0; // 0 = no frame scheduled
  return {
    call() {
      if (pending !== 0) return;
      pending = raf(() => {
        pending = 0;
        fn();
      });
    },
    cancel() {
      if (pending !== 0) {
        caf(pending);
        pending = 0;
      }
    },
  };
}

/**
 * Report the browser-area rect to the backend so the native child WebView2 can
 * be positioned/resized over it. The child webview is a separate OS-level HWND
 * composited above the app's HTML — it renders over the rect this hook reports.
 *
 * Reports on mount + on every `ResizeObserver` tick of `areaRef` + on window
 * `resize`. Coordinates are physical pixels (CSS px × `devicePixelRatio`),
 * window-relative (`getBoundingClientRect` is already viewport-relative, which
 * for a child HWND in the same window is the right frame).
 *
 * Resize storms (an RDP session switch changes geometry/DPI in a burst) are
 * coalesced to ONE `browser_webview_set_rect` per animation frame via
 * [`rafCoalesce`] — a storm cannot flood the backend with WebView2 controller
 * calls, and while the window is occluded rAF never fires so no IPC is
 * emitted at all (the rect only matters when visible). R3 of
 * .coding/reviews/2026-08-18-rdp-freeze-diagnosis.md.
 *
 * Returns nothing; the side effect is the IPC call. The hook is a no-op when
 * `areaRef.current` is null (the ref isn't attached yet) or when `enabled` is
 * false (the Browser tab is unsupported on this platform — nothing to report).
 *
 * @param areaRef a ref to the placeholder div the child webview renders above.
 * @param enabled whether the child webview exists on this platform (Windows
 *        only); when false the hook neither reports nor observes.
 */
export function useBrowserRect(
  areaRef: RefObject<HTMLDivElement | null>,
  enabled = true,
): void {
  useEffect(() => {
    if (!enabled) return;
    const el = areaRef.current;
    if (!el) return;

    /** Read the rect + push it to the backend (physical px, window-relative). */
    function reportRect() {
      const node = areaRef.current;
      if (!node) return;
      const r = node.getBoundingClientRect();
      // Guard against a zero rect (the tab is hidden / display:none) — the
      // backend's set_rect is a no-op on a 0×0 rect anyway, but skip the IPC
      // round-trip entirely.
      if (r.width === 0 || r.height === 0) return;
      const dpr = window.devicePixelRatio || 1;
      void browserWebviewSetRect(
        Math.round(r.x * dpr),
        Math.round(r.y * dpr),
        Math.round(r.width * dpr),
        Math.round(r.height * dpr),
      );
    }

    // Report once on mount so the child webview is positioned immediately.
    reportRect();

    // Coalesce resize-storm ticks to one IPC per animation frame (R3).
    const throttled = rafCoalesce(
      reportRect,
      (cb) => requestAnimationFrame(cb),
      cancelAnimationFrame,
    );

    // Track the placeholder's size (panel resize, tab layout changes).
    const ro = new ResizeObserver(() => throttled.call());
    ro.observe(el);

    // Track window resizes (the placeholder moves even if its own size is
    // stable — e.g. the window shrinks and the layout shifts).
    const onWindowResize = () => throttled.call();
    window.addEventListener("resize", onWindowResize);

    return () => {
      ro.disconnect();
      window.removeEventListener("resize", onWindowResize);
      throttled.cancel();
    };
  }, [areaRef, enabled]);
}
