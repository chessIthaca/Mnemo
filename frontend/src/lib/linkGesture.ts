// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The chat-link escape hatch's PRESS record: ctrl/cmd-click — or middle-click —
 * on an http link opens the OS default browser instead of the app's own
 * Browser tab (see `openChatLink`).
 *
 * WHY THE PRESS AND NOT THE CLICK: the escape hatch used to be decided from
 * the click event's `e.ctrlKey || e.metaKey` alone (MarkdownLink.tsx,
 * Message.tsx). That flag cannot be trusted: the engine decides a
 * ctrl/middle-click gesture itself — WebView2 raises `NewWindowRequested` for
 * it, and wry only surfaces that to the host when a `new_window_req_handler`
 * is registered (this app registers none) — so the `click` the page finally
 * receives can arrive with the modifier already consumed. The reported
 * symptom (user report 2026-10-01): ctrl-click behaved exactly like a plain
 * click and loaded the app's Browser tab, while middle-click — which never
 * reads a modifier — kept working. The PRESS event precedes the engine's
 * gesture decision and always carries the real modifier state, so the hatch is
 * sampled there and consumed once by the click.
 *
 * `preventDefault()` on the press (callers do it when this module reports the
 * hatch armed) additionally suppresses the native gesture at its source.
 *
 * ONE MODULE-SCOPE RECORD rather than a per-element ref: both link surfaces
 * share it — the markdown anchor (MarkdownLink.tsx) and the web_fetch URL chip,
 * which Message.tsx renders inside a `.map` and so cannot own a ref per chip.
 * Every press overwrites the record, so no stale flag can survive to misroute
 * a later click, and `takeLinkPress` clears it so one click consumes exactly
 * one press.
 */

/** Whether the most recent press asked for the OS browser. */
let pressWantsOsBrowser = false;

/**
 * Sample the escape hatch from a PRESS event (`mousedown`/`pointerdown`),
 * where the modifier state is reliable — ctrl/cmd held, or the middle button.
 * Records the result (overwriting any previous press) and returns it, so the
 * caller can `preventDefault()` when the hatch is armed.
 */
export function noteLinkPress(e: {
  ctrlKey: boolean;
  metaKey: boolean;
  button?: number;
}): boolean {
  pressWantsOsBrowser = e.ctrlKey || e.metaKey || e.button === 1;
  return pressWantsOsBrowser;
}

/**
 * Consume the press record for a click: returns whether the press asked for
 * the OS browser (the click itself may carry no modifier — see the module
 * doc) and clears it, so the next click falls back to its own flags.
 */
export function takeLinkPress(): boolean {
  const wanted = pressWantsOsBrowser;
  pressWantsOsBrowser = false;
  return wanted;
}
