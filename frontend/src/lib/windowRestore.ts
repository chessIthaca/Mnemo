// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Startup window-geometry clamping (pure).
 *
 * Bug (user report 2027-01-14): "the app start for this one was a tiny small
 * window top left on the screen". Mnemo persists its window geometry on close
 * and re-applies it on mount (App.tsx) — but it applied the saved values
 * VERBATIM. The Rust-side `min_inner_size(800, 560)` (src-tauri/src/main.rs)
 * does not protect the restore path: tao keeps that constraint only as a
 * `WM_GETMINMAXINFO` `ptMinTrackSize` (vendor/tao/src/platform_impl/windows/
 * event_loop.rs — Windows' *tracking* size, which bounds user-driven sizing),
 * while the restore's programmatic resize goes `set_inner_size` →
 * `set_inner_size_physical` → an explicit `SetWindowPos` that never consults
 * the constraints (…/windows/window.rs, …/windows/util.rs). A geometry saved
 * under a different monitor/DPI layout (RDP resolution change, monitor
 * unplugged, …) therefore came back unusably small and/or partly off screen.
 *
 * This module clamps such a geometry at RESTORE time only:
 *   - size into [min … anchor-monitor work area]; the min wins when the work
 *     area itself is smaller than the min (such a window cannot fit that
 *     screen at any size, and the min is the size the app is known to work
 *     at);
 *   - the saved OUTER size is converted to an inner size via the measured
 *     decoration chrome — the save records outer bounds while the restore
 *     sets an inner (client) size, which tao expands to the frame — so
 *     without the conversion a window would GROW by the title-bar/border
 *     height on every restart;
 *   - the position is kept only when the saved rect clears the existing
 *     usability gate (MIN_VISIBLE_PX logical px on both axes of some
 *     monitor), and is then pulled fully inside that monitor's work area.
 *     When no monitor clears the gate, the clamped window is CENTERED in the
 *     sizing anchor's work area instead — the OS default origin comes from
 *     the INITIAL window size and is not re-centered for the clamped one, so
 *     it can leave the window clipping a screen edge.
 *
 * Only a total monitor-enumeration failure returns a null position — the
 * caller then keeps the OS default. Fully visible geometries pass through
 * unchanged, and nothing here constrains interactive resizing — the Rust-side
 * min stays authoritative there.
 *
 * Pure module: no `@tauri-apps/api` imports, so it runs under vitest's node
 * environment. Tauri's `Monitor` objects match MonitorGeometry structurally,
 * so `await availableMonitors()` can be passed straight in.
 */

/**
 * Minimum restored inner width in logical px — mirrors `min_inner_size` in
 * src-tauri/src/main.rs. The mirror is deliberately PROSE-ONLY (the values are
 * re-stated in the module docs + docs/FEATURES.md): pinning it from here would
 * need a cross-root `?raw` import and a vite `server.fs.allow` escape, for a
 * constant that changes only when the app's real minimum changes.
 */
export const RESTORE_MIN_WIDTH = 800;

/** Minimum restored inner height in logical px — mirrors `min_inner_size` in src-tauri/src/main.rs. */
export const RESTORE_MIN_HEIGHT = 560;

/**
 * Usability gate for a saved position: the saved rect must overlap some
 * monitor by this many LOGICAL px on BOTH axes to keep its position (title
 * bar + a bit) — the same margin the pre-clamp restore used, which compared
 * `80 * factor` PHYSICAL px.
 */
export const MIN_VISIBLE_PX = 80;

/**
 * A persisted geometry, in logical px. `width`/`height` are the OUTER frame
 * size (what the save path stores); the clamp converts them to an inner size.
 */
export interface SavedGeometry {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** A rect in physical px. */
interface PhysRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The slice of Tauri's `Monitor` this module needs (structural mirror). */
export interface MonitorGeometry {
  position: { x: number; y: number };
  size: { width: number; height: number };
  scaleFactor: number;
  /** Usable area (excludes taskbar/dock); absent/null on runtimes that don't report it. */
  workArea?:
    | {
        position: { x: number; y: number };
        size: { width: number; height: number };
      }
    | null;
}

/** Inputs for {@link clampRestoredGeometry}. */
export interface ClampInputs {
  /** The geometry read back from localStorage. */
  saved: SavedGeometry;
  /** The window's current scale factor (saved coords are logical). */
  factor: number;
  /** Known monitors, `await availableMonitors()`. */
  monitors: MonitorGeometry[];
  /** Decoration chrome = outer − inner, in physical px; {0,0} when unknown. */
  chrome: { width: number; height: number };
  /** Overrides {@link MIN_VISIBLE_PX}, in logical px (compared as × factor). */
  minOverlapPx?: number;
}

/**
 * The clamped restore target: an inner size in logical px, plus a logical
 * position. `x`/`y` are `null` only when NO monitor was known at all (an
 * unavailable enumeration) — the caller then leaves the window at the OS
 * default. Whenever a monitor IS known the position is a fully visible one:
 * the saved position clamped into that monitor's work area, or that work
 * area's center when the saved rect failed the usability gate.
 */
export interface ClampedGeometry {
  width: number;
  height: number;
  x: number | null;
  y: number | null;
}

/** One monitor's work area, scored against the saved rect. */
interface Scored {
  area: PhysRect;
  overlapX: number;
  overlapY: number;
  overlapArea: number;
  /** Clears the usability gate on both axes. */
  usable: boolean;
}

/** Clamp `v` into `[lo, hi]`. */
function clampNumber(v: number, lo: number, hi: number): number {
  return Math.min(Math.max(v, lo), hi);
}

/** A monitor's usable rect: its work area when reported, else the full rect. */
function monitorRect(m: MonitorGeometry): PhysRect {
  const wa = m.workArea; // runtime feature-detect — not every runtime reports it
  const pos = wa ? wa.position : m.position;
  const size = wa ? wa.size : m.size;
  return { x: pos.x, y: pos.y, width: size.width, height: size.height };
}

/**
 * Clamp a persisted window geometry into something usable on the current
 * monitor layout. See the module docs for the exact rules: the size is
 * clamped into `[RESTORE_MIN_* … anchor work area]`, and the position is a
 * fully visible one whenever any monitor is known. Returns logical px,
 * rounded; `x`/`y` are `null` only with no monitors at all.
 */
export function clampRestoredGeometry(inputs: ClampInputs): ClampedGeometry {
  const { saved, monitors, chrome } = inputs;
  // Defensive: a non-positive / non-finite factor would turn every returned
  // logical value into Infinity or NaN (and the IPC would reject them).
  const factor = Number.isFinite(inputs.factor) && inputs.factor > 0 ? inputs.factor : 1;
  // The gate is a LOGICAL-px margin, so it scales with the factor exactly as
  // the pre-clamp restore's `80 * factor` comparison did.
  const minOverlap = (inputs.minOverlapPx ?? MIN_VISIBLE_PX) * factor;

  const savedRect: PhysRect = {
    x: saved.x * factor,
    y: saved.y * factor,
    width: saved.width * factor,
    height: saved.height * factor,
  };

  const scored: Scored[] = monitors.map((m) => {
    // NOTE: each monitor's own scaleFactor is deliberately not consulted — the
    // saved rect is interpreted with the WINDOW's factor, the same
    // approximation the pre-clamp restore made (pinned by the mixed-DPI case).
    const area = monitorRect(m);
    const overlapX =
      Math.min(savedRect.x + savedRect.width, area.x + area.width) - Math.max(savedRect.x, area.x);
    const overlapY =
      Math.min(savedRect.y + savedRect.height, area.y + area.height) - Math.max(savedRect.y, area.y);
    return {
      area,
      overlapX,
      overlapY,
      overlapArea: Math.max(0, overlapX) * Math.max(0, overlapY),
      usable: overlapX >= minOverlap && overlapY >= minOverlap,
    };
  });
  const byOverlapArea = (a: Scored, b: Scored): number => b.overlapArea - a.overlapArea;
  const usable = scored.filter((s) => s.usable).sort(byOverlapArea);
  // Sizing anchor: the monitor the saved rect most belongs to (falling back to
  // the best overlap overall when none clears the gate, so an off-screen save
  // still gets a sensible work-area bound); position anchor: a usable monitor
  // only — that is what decides whether the saved position is kept.
  const sizing = usable[0] ?? scored.slice().sort(byOverlapArea)[0] ?? null;
  const positioning = usable[0] ?? null;

  // Guarded chrome: a bogus measurement (or a saved size smaller than the
  // chrome) must never yield a negative inner size.
  const chromeW = savedRect.width > chrome.width ? Math.max(0, chrome.width) : 0;
  const chromeH = savedRect.height > chrome.height ? Math.max(0, chrome.height) : 0;
  const innerW = Math.max(savedRect.width - chromeW, 1);
  const innerH = Math.max(savedRect.height - chromeH, 1);

  const minW = RESTORE_MIN_WIDTH * factor;
  const minH = RESTORE_MIN_HEIGHT * factor;
  // No known monitor: bound the size by the minimum only — a failed
  // enumeration must never SHRINK an otherwise good save (the position is
  // dropped on that path anyway).
  const maxW = sizing ? Math.max(minW, sizing.area.width - chromeW) : Number.POSITIVE_INFINITY;
  const maxH = sizing ? Math.max(minH, sizing.area.height - chromeH) : Number.POSITIVE_INFINITY;

  const widthPhys = clampNumber(innerW, minW, maxW);
  const heightPhys = clampNumber(innerH, minH, maxH);
  const width = Math.round(widthPhys / factor);
  const height = Math.round(heightPhys / factor);

  if (!sizing) return { width, height, x: null, y: null };

  const area = (positioning ?? sizing).area;
  // Keep the saved position when it cleared the gate; otherwise CENTER the
  // clamped window in the work area, so "fully on screen" holds there too.
  const wantedX = positioning ? savedRect.x : area.x + (area.width - (widthPhys + chromeW)) / 2;
  const wantedY = positioning ? savedRect.y : area.y + (area.height - (heightPhys + chromeH)) / 2;
  const maxX = Math.max(area.x, area.x + area.width - (widthPhys + chromeW));
  const maxY = Math.max(area.y, area.y + area.height - (heightPhys + chromeH));
  return {
    width,
    height,
    x: Math.round(clampNumber(wantedX, area.x, maxX) / factor),
    y: Math.round(clampNumber(wantedY, area.y, maxY) / factor),
  };
}
