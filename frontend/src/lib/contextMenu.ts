// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Suppresses the webview's native right-click (context) menu across the Mnemo
 * UI, except where it is useful: inside editable text fields (Copy/Cut/Paste)
 * and over an ACTIVE TEXT SELECTION (backlog 8863159f: "I want to select
 * things in the agent window so I can copy them" — transcript/tool-output
 * text drag-selects fine, but with the menu suppressed everywhere there was no
 * way to Copy it; a right-click over the selected text now keeps the native
 * menu so its Copy item works).
 *
 * The Browser tab is a separate native child WebView2 (its own OS webview
 * instance), so its context menu is independent of this guard — it keeps its
 * full menu with no special handling. This guard only affects the agent-chat
 * webview that hosts the React app.
 */

/**
 * Pure, DOM-free decision: is a context-menu target editable, given its tag
 * name and the `contenteditable` attribute value of its nearest host?
 *
 * A target is editable when it is an `<input>` / `<textarea>`, or when its
 * nearest `[contenteditable]` ancestor has the attribute set to `""`,
 * `"true"`, or `"plaintext-only"` (the values that make a host editable per
 * the HTML spec). `"false"`, `"inherit"`, and a missing attribute are NOT
 * editable.
 *
 * Extracted from {@link isEditableTarget} so the attribute-value semantics
 * can be unit-tested without a DOM.
 */
export function isEditableContext(
  tag: string | undefined,
  contentEditableAttr: string | null,
): boolean {
  if (tag === "INPUT" || tag === "TEXTAREA") return true;
  // The `contenteditable` enumerated attribute matches its keywords
  // case-insensitively per the HTML spec, so normalize before comparing.
  const a = contentEditableAttr?.toLowerCase() ?? null;
  return a === "" || a === "true" || a === "plaintext-only";
}

/**
 * Resolve a `contextmenu` event target to its owning Element, or `null`.
 *
 * A right-click on selected text inside a contenteditable host targets the
 * `Text` node, not the host — resolve to its parent element so the editable
 * check still succeeds.
 */
function asElement(target: EventTarget | null): Element | null {
  if (target instanceof Element) return target;
  if (target instanceof Text) return target.parentElement;
  return null;
}

/**
 * Does `target` sit inside an editable field (so its context menu should be
 * kept)? Walks up to the nearest `[contenteditable]` host and delegates the
 * attribute-value decision to {@link isEditableContext}.
 */
export function isEditableTarget(target: EventTarget | null): boolean {
  const el = asElement(target);
  if (!el) return false;
  const host = el.closest("[contenteditable]");
  return isEditableContext(
    el.tagName,
    host ? host.getAttribute("contenteditable") : null,
  );
}

/**
 * Pure, DOM-free decision: should the native context menu be KEPT for a
 * right-click? Kept when the target is an editable field (Copy/Cut/Paste) or
 * when the click lands over an active text selection (the user selected
 * transcript text to copy — the native menu's Copy item serves it).
 * Suppressed everywhere else.
 */
export function keepNativeMenu(isEditable: boolean, overSelection: boolean): boolean {
  return isEditable || overSelection;
}

/**
 * Does the active text selection (a) exist, (b) have content (not collapsed),
 * and (c) include the right-click's target element? The right-click target
 * for a click on selected text is the text node's parent element, and
 * `containsNode(el, true)` allows partial containment (a selection may start
 * or end mid-element), so the check is tolerant of the exact node the event
 * resolves to. Supported in both of the app's webviews (Chromium + WebKit).
 */
export function activeSelectionIncludes(target: EventTarget | null): boolean {
  if (typeof document === "undefined") return false;
  const sel = document.getSelection();
  if (!sel || sel.isCollapsed || sel.rangeCount === 0) return false;
  const el = asElement(target);
  if (!el) return false;
  return sel.containsNode(el, true);
}

/**
 * Install a capture-phase `contextmenu` listener on `document` that calls
 * `preventDefault()` — suppressing the webview's native context menu —
 * unless the target is an editable field (see [`isEditableTarget`]) or the
 * click lands over an active text selection (see
 * [`activeSelectionIncludes`], so the native Copy item stays reachable for
 * selected transcript text).
 *
 * Capture phase is used so the guard runs before any component-level handler
 * and reliably sees every contextmenu event in the app. Returns a cleanup
 * function that removes the listener.
 */
export function installContextMenuGuard(): () => void {
  const onContextMenu = (e: MouseEvent): void => {
    if (keepNativeMenu(isEditableTarget(e.target), activeSelectionIncludes(e.target))) {
      return;
    }
    e.preventDefault();
  };
  document.addEventListener("contextmenu", onContextMenu, true);
  return () => {
    document.removeEventListener("contextmenu", onContextMenu, true);
  };
}
