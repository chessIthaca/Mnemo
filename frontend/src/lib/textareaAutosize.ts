// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Shared auto-resize math for prompt-style textareas (InputBar, Backlog).
 *
 * Why this exists (the phantom-scrollbar bug): under Tailwind's global
 * `box-sizing: border-box`, setting `height = scrollHeight` leaves the
 * layout ~border-width short (`scrollHeight` excludes borders), so with the
 * UA-default `overflow: auto` a vertical scrollbar thumb rendered
 * permanently — the gray sliver at the right edge of the prompt box, even
 * when empty. The fix: only switch overflow back to `auto` once the height
 * is actually clamped (content genuinely cannot fit); below the cap the box
 * always fits its content, so overflow can stay `hidden`.
 */

/**
 * Compute the height + `overflowY` for an auto-resizing textarea measured
 * via the collapse trick (`height: "auto"` first, then read `scrollHeight`).
 *
 * @param scrollHeight The textarea's measured `scrollHeight` (pixels), with
 *   the element collapsed to `auto` height so it reflects the content.
 * @param maxPx The maximum grown height (the scroll cap, e.g. 200 for the
 *   prompt input). At or below the cap the content fits, so `overflowY` is
 *   `"hidden"` — the box-sizing border shortfall is absorbed by bottom
 *   padding, never clipping text. Above it, the height clamps and
 *   `overflowY` is `"auto"` so long input still scrolls.
 * @returns The pixel height to apply and the `overflowY` value to set.
 */
export function autosizeForScrollHeight(
  scrollHeight: number,
  maxPx: number,
): { heightPx: number; overflowY: "auto" | "hidden" } {
  const heightPx = Math.min(scrollHeight, maxPx);
  return { heightPx, overflowY: scrollHeight > maxPx ? "auto" : "hidden" };
}
