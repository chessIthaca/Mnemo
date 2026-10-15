// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The chat-link escape hatch's press record (src/lib/linkGesture.ts).
 *
 * Regression for the reported symptom (user report 2026-10-01): a ctrl-click
 * on an http link loaded the app's own Browser tab instead of the OS browser —
 * the escape hatch was lost. Root cause: the hatch was decided from the CLICK
 * event's `e.ctrlKey || e.metaKey` alone, but the engine decides the
 * ctrl/middle-click gesture itself (WebView2 raises NewWindowRequested; the
 * app registers no new-window handler), so the click can arrive with the
 * modifier already consumed. The press is the reliable seam — the first case
 * below is exactly the symptom and cannot be satisfied by the old click-only
 * expression. Middle-click needs no modifier at all (case 3), which is why it
 * kept working while ctrl-click did not.
 *
 * Node-env suite (no DOM): the wiring itself is pinned by the `?raw` source
 * contracts in src/components/chat/MarkdownLink.test.tsx. The cases are NAMED
 * functions (not inline `it` callbacks) so the code graph — and the plan's
 * regression-test record — can point at them.
 */

import { beforeEach, describe, expect, it } from "vitest";

import { noteLinkPress, takeLinkPress } from "./linkGesture";

/**
 * The reported symptom: ctrl was held on the PRESS, and the click event the
 * engine dispatches afterwards reports no modifier. The hatch must survive
 * that — the old click-only expression read false here and sent the click into
 * the app's Browser tab.
 */
function keepsTheEscapeHatchWhenTheClickCarriesNoModifier(): void {
  expect(noteLinkPress({ ctrlKey: true, metaKey: false })).toBe(true);
  expect(takeLinkPress()).toBe(true);
}

/** macOS cmd-click takes the same path as ctrl-click. */
function keepsTheEscapeHatchForACmdClick(): void {
  expect(noteLinkPress({ ctrlKey: false, metaKey: true })).toBe(true);
  expect(takeLinkPress()).toBe(true);
}

/** Middle-click needs no modifier at all — which is why it never broke. */
function keepsTheEscapeHatchForAMiddleButtonPress(): void {
  expect(noteLinkPress({ ctrlKey: false, metaKey: false, button: 1 })).toBe(true);
  expect(takeLinkPress()).toBe(true);
}

/** A plain press — and a click with no press — stay in the app's Browser tab. */
function keepsAPlainPressOnTheAppsBrowserTab(): void {
  expect(noteLinkPress({ ctrlKey: false, metaKey: false, button: 0 })).toBe(false);
  expect(takeLinkPress()).toBe(false);
}

/** The hatch does not leak into the following click. */
function consumesExactlyOnePressPerClick(): void {
  noteLinkPress({ ctrlKey: true, metaKey: false });
  expect(takeLinkPress()).toBe(true);
  expect(takeLinkPress()).toBe(false);
}

/** A stale press cannot misroute a later click. */
function letsTheNextPressOverwriteAStaleOne(): void {
  noteLinkPress({ ctrlKey: true, metaKey: false });
  expect(noteLinkPress({ ctrlKey: false, metaKey: false, button: 0 })).toBe(false);
  expect(takeLinkPress()).toBe(false);
}

describe("linkGesture", () => {
  beforeEach(() => {
    // Drain any press left behind by a previous case.
    takeLinkPress();
  });

  it(
    "keeps the escape hatch for a ctrl-click whose click carries no modifier",
    keepsTheEscapeHatchWhenTheClickCarriesNoModifier,
  );

  it("keeps the escape hatch for a cmd-click (macOS)", keepsTheEscapeHatchForACmdClick);

  it("keeps the escape hatch for a middle-button press", keepsTheEscapeHatchForAMiddleButtonPress);

  it(
    "keeps a plain press — and a click with no press — on the app's Browser tab",
    keepsAPlainPressOnTheAppsBrowserTab,
  );

  it("consumes exactly one press per click", consumesExactlyOnePressPerClick);

  it("lets the next press overwrite a stale one", letsTheNextPressOverwriteAStaleOne);
});
