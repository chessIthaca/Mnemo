// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Unit tests for the pure context-menu decision in `lib/contextMenu.ts`.
 *
 * `isEditableContext` encodes which `contenteditable` attribute values make a
 * host editable (per the HTML spec: `""`, `"true"`, `"plaintext-only"`) and
 * which native tags are always editable (`<input>` / `<textarea>`). The
 * DOM-facing `isEditableTarget` is thin glue over this pure function.
 */

import { describe, expect, it } from "vitest";

import { isEditableContext, keepNativeMenu } from "./contextMenu";

describe("isEditableContext", () => {
  it.each<[string | undefined, string | null, boolean]>([
    // Native editable controls keep their menu regardless of contenteditable.
    ["INPUT", null, true],
    ["TEXTAREA", null, true],
    // contenteditable hosts: the three editable values.
    ["DIV", "true", true],
    ["DIV", "", true],
    ["DIV", "plaintext-only", true],
    // Enumerated attribute keywords match case-insensitively (HTML spec).
    ["DIV", "TRUE", true],
    ["DIV", "Plaintext-Only", true],
    // A span inside a contenteditable host resolves to the host's attr.
    ["SPAN", "true", true],
    // Non-editable attribute values.
    ["DIV", "false", false],
    ["DIV", "inherit", false],
    ["DIV", null, false],
    // No tag and no host.
    [undefined, null, false],
  ])("tag=%j attr=%j -> editable=%s", (tag, attr, expected) => {
    expect(isEditableContext(tag, attr)).toBe(expected);
  });
});

describe("keepNativeMenu", () => {
  // The native context menu is kept only where it serves the user: editable
  // fields (Copy/Cut/Paste) and active text selections (the user selected
  // transcript text to copy — backlog 8863159f). Suppressed everywhere else,
  // keeping the app-chrome menu-free default.
  it.each<[boolean, boolean, boolean]>([
    // Plain right-click on UI chrome — no selection, not editable.
    [false, false, false],
    // Editable field (input/textarea/contenteditable) keeps its menu.
    [true, false, true],
    // Right-click over an active text selection keeps the menu (Copy).
    [false, true, true],
    // Both (a selection inside an editable field) — kept either way.
    [true, true, true],
  ])("editable=%j overSelection=%j -> keep=%s", (editable, overSelection, expected) => {
    expect(keepNativeMenu(editable, overSelection)).toBe(expected);
  });
});
