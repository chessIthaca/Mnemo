// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * windowRestore — regression tests for the startup window-geometry clamp.
 *
 * Defect (user report 2027-01-14): "the app start for this one was a tiny
 * small window top left on the screen". Mnemo persists its size + position
 * (localStorage `mh.windowGeometry`, outer bounds in logical px) and
 * re-applies them on mount — but it applied them VERBATIM. The Rust-side
 * `min_inner_size(800, 560)` (src-tauri/src/main.rs) only constrains
 * interactive resizing, never the restore's programmatic `setSize()`, so a
 * geometry saved under a different monitor/DPI layout (e.g. an RDP
 * resolution change: 1000×700 saves as 500×350 at 2×) restored as an
 * unusably small window. The old position gate only asked "does the saved
 * rect overlap SOME monitor by 80px?" — an on-screen but mostly-off-screen
 * or near-origin rect sailed through.
 *
 * The fix clamps at restore time only (never during resize): size into
 * [min … anchor work area], position fully inside that work area (centered in
 * it when the saved rect fails the usability gate), with the saved OUTER size
 * converted to an inner size via the measured decoration chrome. The pure
 * helper lives in ./windowRestore (no @tauri-apps/api imports, so these tests
 * run in the node env); the wiring is pinned as source contracts on App.tsx,
 * in the style of delegationNotes.test.ts.
 *
 * Case mix (review round 1 of plan 9ff59133 asked for the coverage below):
 * the user-reported defect (a tiny save bumped to the minimum), the size
 * rules incl. factor + chrome together, the min floor over a smaller work
 * area with non-zero chrome, and the no-monitor-known path; the position
 * rules incl. a negative-coordinate monitor, the mixed-DPI approximation,
 * hand-edited zero/negative saves, and the minOverlapPx override.
 */

import { describe, expect, it } from "vitest";
import {
  MIN_VISIBLE_PX,
  RESTORE_MIN_HEIGHT,
  RESTORE_MIN_WIDTH,
  clampRestoredGeometry,
  type ClampInputs,
  type MonitorGeometry,
} from "./windowRestore";
import appSource from "../App.tsx?raw";

/** One monitor fixture, scaleFactor 1 unless overridden. */
function mon(
  x: number,
  y: number,
  width: number,
  height: number,
  extra: Partial<MonitorGeometry> = {},
): MonitorGeometry {
  return { position: { x, y }, size: { width, height }, scaleFactor: 1, ...extra };
}

/** The common single-1080p case: origin monitor, no chrome, factor 1. */
function clamp(partial: Partial<ClampInputs> & Pick<ClampInputs, "saved">) {
  return clampRestoredGeometry({
    factor: 1,
    monitors: [mon(0, 0, 1920, 1080)],
    chrome: { width: 0, height: 0 },
    ...partial,
  });
}

describe("clampRestoredGeometry — size", () => {
  it("leaves a fully visible geometry untouched", () => {
    expect(clamp({ saved: { x: 100, y: 100, width: 1000, height: 700 } })).toEqual({
      width: 1000,
      height: 700,
      x: 100,
      y: 100,
    });
  });

  it("bumps a tiny saved size up to the minimum (user report 2027-01-14)", () => {
    // Saved 300×200 at the origin — the reported "tiny small window top
    // left on the screen" after a monitor-layout change.
    const out = clamp({ saved: { x: 0, y: 0, width: 300, height: 200 } });
    expect(out.width).toBe(RESTORE_MIN_WIDTH);
    expect(out.height).toBe(RESTORE_MIN_HEIGHT);
    expect(out).toEqual({ width: 800, height: 560, x: 0, y: 0 });
  });

  it("shrinks a size larger than the anchor work area to fit", () => {
    const out = clamp({ saved: { x: 0, y: 0, width: 2000, height: 1200 } });
    expect(out).toEqual({ width: 1920, height: 1080, x: 0, y: 0 });
  });

  it("lets the minimum floor win over a work area smaller than the minimum", () => {
    const out = clamp({
      monitors: [mon(0, 0, 640, 480)],
      saved: { x: 0, y: 0, width: 300, height: 200 },
    });
    expect(out).toEqual({ width: 800, height: 560, x: 0, y: 0 });
  });

  it("applies the min floor with non-zero chrome on a work area below the min", () => {
    // The subtlest branch: the work area (640×480) is smaller than the
    // minimum, so maxW/maxH collapse onto the floor — and the position must
    // still resolve to the work-area origin (the title bar stays grabbable)
    // even though width + chrome now overshoots the area.
    const out = clamp({
      monitors: [mon(0, 0, 640, 480)],
      chrome: { width: 16, height: 39 },
      saved: { x: 0, y: 0, width: 300, height: 200 },
    });
    expect(out).toEqual({ width: 800, height: 560, x: 0, y: 0 });
  });

  it("converts the saved OUTER size to an inner size via the chrome", () => {
    // Legacy save under a decorated window: outer = inner + {16, 39}.
    const out = clamp({
      chrome: { width: 16, height: 39 },
      saved: { x: 100, y: 100, width: 800 + 16, height: 560 + 39 },
    });
    expect(out).toEqual({ width: 800, height: 560, x: 100, y: 100 });
  });

  it("clamps in physical px and returns logical px", () => {
    // factor 2 (the 1000×700 → 500×350 save: 500 logical = 1000 physical).
    const out = clamp({
      factor: 2,
      monitors: [mon(0, 0, 3840, 2160)],
      saved: { x: 100, y: 100, width: 400, height: 300 },
    });
    expect(out).toEqual({ width: 800, height: 560, x: 100, y: 100 });
  });

  it("applies factor and chrome together (a real cross-DPI restart)", () => {
    // Both corrections at once: outer 808×579 logical at factor 2 with the
    // {16, 39} chrome → inner 1600×1119 physical, where the WIDTH already
    // meets the minimum (1600 = 800 × 2) and the HEIGHT is floored up to
    // 1120 (560 × 2) — i.e. no double-subtraction of the chrome.
    const out = clamp({
      factor: 2,
      monitors: [mon(0, 0, 3840, 2160)],
      chrome: { width: 16, height: 39 },
      saved: { x: 100, y: 100, width: 808, height: 579 },
    });
    expect(out).toEqual({ width: 800, height: 560, x: 100, y: 100 });
  });

  it("clamps to the minimum alone when no monitor is known", () => {
    const out = clamp({ monitors: [], saved: { x: 0, y: 0, width: 300, height: 200 } });
    expect(out).toEqual({ width: 800, height: 560, x: null, y: null });
  });

  it("never shrinks a good save when no monitor is known", () => {
    // An unavailable monitor enumeration bounds the size by the minimum only
    // — a large-but-valid save must survive it (the position is dropped on
    // this path anyway, so there is nothing to fit).
    const out = clamp({ monitors: [], saved: { x: 0, y: 0, width: 1600, height: 1000 } });
    expect(out).toEqual({ width: 1600, height: 1000, x: null, y: null });
  });

  it("floors a hand-edited zero-size save and centers it", () => {
    // localStorage is hand-editable: a zero/negative rect must not produce a
    // degenerate window (Math.max(…, 1) then the min clamp), and a rect with
    // no usable overlap is centered in the work area rather than left off it.
    const out = clamp({ saved: { x: -50, y: -50, width: 0, height: 0 } });
    expect(out).toEqual({ width: 800, height: 560, x: 560, y: 260 });
  });
});

describe("clampRestoredGeometry — position", () => {
  it("pulls a partially off-screen position fully inside the work area", () => {
    // Overlaps the monitor by exactly the usable margin (80px) in both
    // axes — the boundary still qualifies (>=), and the position is then
    // clamped so the whole window fits on screen.
    const out = clamp({ saved: { x: 1700, y: 1000, width: 400, height: 300 } });
    expect(out).toEqual({ width: 800, height: 560, x: 1120, y: 520 });
  });

  it("centers the clamped window when the overlap is below the usable margin", () => {
    // 60px of overlap on X — too little to keep: the window is centered in
    // the work area instead. NOT null (that is the no-monitor case): the OS
    // default origin comes from the initial 1200×720 window and is not
    // re-centered for the clamped size, so it could clip the screen edge.
    const out = clamp({
      saved: { x: 1920 - 60, y: 100, width: 1000, height: 700 },
    });
    expect(out).toEqual({ width: 1000, height: 700, x: 460, y: 190 });
    expect(MIN_VISIBLE_PX).toBe(80);
  });

  it("anchors to the monitor with the largest overlap", () => {
    // Spans the seam at x=1920: 20px on monitor 1 (below the gate), 880px
    // on monitor 2 — so monitor 2's work area wins and the window lands
    // inside it.
    const out = clamp({
      monitors: [mon(0, 0, 1920, 1080), mon(1920, 0, 2560, 1440)],
      saved: { x: 1900, y: 100, width: 900, height: 800 },
    });
    expect(out).toEqual({ width: 900, height: 800, x: 1920, y: 100 });
  });

  it("keeps a position on a monitor at negative coordinates", () => {
    // A display left of the primary: the saved x is negative, the work area
    // runs -1920…0, and the window must stay there (no clamp to 0).
    const out = clamp({
      monitors: [mon(-1920, 0, 1920, 1080)],
      saved: { x: -1800, y: 50, width: 1000, height: 700 },
    });
    expect(out).toEqual({ width: 1000, height: 700, x: -1800, y: 50 });
  });

  it("uses the window's factor for the saved rect, ignoring per-monitor DPI", () => {
    // Mixed-DPI layout (second display at 2×): the clamp deliberately does
    // NOT scale the saved rect per monitor — the same approximation the
    // pre-clamp restore made. Pinned so a future "fix" of that is a
    // deliberate decision, not a silent behaviour change.
    const out = clamp({
      monitors: [mon(0, 0, 1920, 1080), mon(1920, 0, 3840, 2160, { scaleFactor: 2 })],
      saved: { x: 2000, y: 100, width: 1000, height: 700 },
    });
    expect(out).toEqual({ width: 1000, height: 700, x: 2000, y: 100 });
  });

  it("honours the minOverlapPx override", () => {
    // Same rect as the boundary case above (80px of Y overlap): a stricter
    // gate flips it from "keep the position" to "center it".
    const out = clamp({
      minOverlapPx: 200,
      saved: { x: 1700, y: 1000, width: 400, height: 300 },
    });
    expect(out).toEqual({ width: 800, height: 560, x: 560, y: 260 });
  });

  it("respects the work area (not the full monitor) when clamping", () => {
    // A 40px taskbar strip at the bottom of an 800-tall monitor: the work
    // area ends at y=800, so a saved bottom edge of 820 is pulled up to
    // 100 (with the full monitor rect it would stay at 120).
    const withWorkArea = clamp({
      monitors: [
        mon(100, 50, 1000, 800, { workArea: { position: { x: 100, y: 90 }, size: { width: 1000, height: 710 } } }),
      ],
      saved: { x: 100, y: 120, width: 1000, height: 700 },
    });
    expect(withWorkArea).toEqual({ width: 1000, height: 700, x: 100, y: 100 });

    // No work area reported (older platform shim): the monitor rect stands in.
    const withoutWorkArea = clamp({
      monitors: [mon(100, 50, 1000, 800)],
      saved: { x: 100, y: 120, width: 1000, height: 700 },
    });
    expect(withoutWorkArea).toEqual({ width: 1000, height: 700, x: 100, y: 120 });
  });
});

describe("App.tsx restore applies the clamp (source contract)", () => {
  it("imports and calls the clamp helper", () => {
    expect(appSource).toContain('import { clampRestoredGeometry } from "./lib/windowRestore";');
    expect(appSource).toContain("clampRestoredGeometry({");
  });

  it("never re-applies the saved size verbatim", () => {
    expect(appSource).not.toContain("new LogicalSize(saved.width, saved.height)");
    expect(appSource).toContain("new LogicalSize(restored.width, restored.height)");
  });

  it("measures the decoration chrome for the outer→inner conversion", () => {
    expect(appSource).toContain("outerSize()");
    expect(appSource).toContain("innerSize()");
  });

  it("skips setPosition only when no monitor was known", () => {
    // The guard names BOTH halves: the helper returns x and y as a pair
    // (null only with no monitors at all), so narrowing the pair keeps
    // LogicalPosition's arguments non-nullable without a cast.
    expect(appSource).toContain("if (restored.x !== null && restored.y !== null)");
    expect(appSource).toContain("new LogicalPosition(restored.x, restored.y)");
  });

  it("constrains the startup restore only — no runtime min size", () => {
    // Scope both checks to the mount effect's restore block: that is the one
    // place allowed to touch the startup size, and it must not impose a
    // RESIZE-TIME minimum (the Rust-side min_inner_size owns that; the user
    // asked for startup-only clamping on 2027-01-14).
    const start = appSource.indexOf("Restore saved geometry once on mount");
    const end = appSource.indexOf("let saveTimer");
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    const restoreBlock = appSource.slice(start, end);
    expect(restoreBlock).toContain("clampRestoredGeometry({");
    expect(restoreBlock).not.toContain("setMinSize");
    // Called exactly once — the import line carries no call parens.
    expect(appSource.split("clampRestoredGeometry(")).toHaveLength(2);
  });

  it("re-maximizes when the saved geometry was maximized", () => {
    // A maximized window used to restart un-maximized at its last normal
    // bounds (the save skipped while maximized, so `maximized: true` was
    // never persisted and the restore guard excluded it) — not the same
    // sizing as the closed window. The restore must apply the clamped
    // normal bounds and then re-maximize.
    const start = appSource.indexOf("Restore saved geometry once on mount");
    const end = appSource.indexOf("let saveTimer");
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    const restoreBlock = appSource.slice(start, end);
    expect(restoreBlock).toContain("if (saved.maximized)");
    expect(restoreBlock).toContain("win.maximize()");
  });

  it("persists the maximized flag instead of skipping the save", () => {
    // The save path must write `maximized: true` on the last normal bounds
    // (and beforeunload must carry the flag) — otherwise the re-maximize
    // above can never fire.
    expect(appSource).toContain("maximized: true");
    expect(appSource).toContain("lastMaximized");
  });

  it("registers the geometry flusher for the switch-restart path", () => {
    // App's geometry effect must register the flusher (and clear it on
    // cleanup) so the picker's pre-restart flush has something to call —
    // the hard process exit never runs beforeunload.
    expect(appSource).toContain("registerWindowGeometryFlusher(() => {");
    expect(appSource).toContain("registerWindowGeometryFlusher(null)");
  });

  it("keeps the maximized flag fresh between debounced saves", () => {
    // Review LOW 2: beforeunload writes the cached flag synchronously (it
    // can't await an IPC round-trip), so a maximize followed by a close
    // within the 400 ms debounce would otherwise persist a stale
    // `maximized: false`. Every move/resize event must refresh the flag
    // fire-and-forget.
    const start = appSource.indexOf("const debouncedSave");
    const end = appSource.indexOf("registerWindowGeometryFlusher(() => {");
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    const block = appSource.slice(start, end);
    expect(block).toContain("isMaximized()");
    expect(block).toContain("lastMaximized = m");
  });
});
