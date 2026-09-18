// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Route a chat link's http(s) target into the app's OWN Browser tab (the
 * right-panel child WebView2) instead of the OS default browser (user request
 * 2027-01-16).
 *
 * The URL travels through the store (`requestBrowserOpen`) rather than being
 * navigated from here, because the child webview's RECT lives in
 * frontend/src/components/views/BrowserView.tsx: this module has no rect to
 * hand `browserWebviewEnsure`, and calling ensure with a guessed one would
 * MOVE an already-created child webview to the wrong place (the backend
 * `browser_webview_ensure` only moves/resizes once the child exists).
 * BrowserView consumes `pendingBrowserUrl` on mount/change and runs its own
 * rect-aware ensure + navigate.
 *
 * Two paths keep the OS browser: an explicit click modifier (ctrl/cmd-click,
 * middle-click — `opts.osBrowser`) and platforms without the child webview
 * (macOS/Linux, where `browserWebviewSupported` is false and the Browser tab
 * renders a red explanation panel instead of a URL bar) — there, shelling out
 * is the only way to see the page at all.
 *
 * Only `http:`/`https:` are routed — the same discipline `openExternal`
 * enforces. Chat links must NOT widen the Browser tab's allow-list (its
 * `normalize_url` choke point also accepts `data:`/`file:`); `mailto:`,
 * fragments and relative paths stay the non-openable text MarkdownLink already
 * renders.
 */

import { useAgentStore } from "../hooks/useAgentStore";
import { openExternal } from "./openExternal";
import { browserWebviewSupported } from "./tauri";

/**
 * Open a chat link's target in the app's Browser tab, or in the OS default
 * browser where that tab cannot exist (or when the caller asks for it).
 *
 * Never throws: a target that is not an absolute http(s) URL is ignored
 * outright (no OS open, no tab navigation).
 *
 * @param url - the raw href from the chat.
 * @param opts.osBrowser - force the OS browser instead of the tab
 *   (ctrl/cmd-click and middle-click keep the pre-2027-01-16 behavior).
 */
export async function openChatLink(
  url: string,
  opts?: { osBrowser?: boolean },
): Promise<void> {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    // Relative paths, fragments, mailto: — MarkdownLink renders these as
    // plain text; the router must not invent an opener for them.
    return;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return;
  // The normalized href is what gets opened/handed on, so odd-but-parseable
  // forms resolve exactly as a browser would (mirrors openExternal).
  if (opts?.osBrowser) {
    await openExternal(parsed.href);
    return;
  }
  // Ask the backend whether the native child webview exists on this platform.
  // A REJECTION (IPC hiccup, or a backend without the command) counts as
  // supported, mirroring BrowserView's deliberately optimistic mount probe:
  // the tab path degrades visibly (its own error panel), while falling back to
  // an OS-browser launch on a transient failure would surprise the user.
  let supported = true;
  try {
    supported = await browserWebviewSupported();
  } catch {
    supported = true;
  }
  if (!supported) {
    // macOS/Linux: opening the tab would only surface its unsupported-platform
    // panel, so keep the pre-existing OS-browser behavior.
    await openExternal(parsed.href);
    return;
  }
  useAgentStore.getState().requestBrowserOpen(parsed.href);
}
