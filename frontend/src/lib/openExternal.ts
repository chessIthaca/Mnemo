// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { open as openUrl } from "@tauri-apps/plugin-shell";

/**
 * Open an external http(s) URL in the user's default browser via the Tauri
 * shell plugin. The app's production CSP (`default-src 'self'`) blocks
 * ordinary `<a target="_blank">` navigation, so external links must be
 * routed through the shell `open` command (granted by `shell:allow-open`).
 * Shared by the About dialog's license/registry links and the web_fetch
 * ToolCard's URL chip (Message.tsx).
 *
 * Only `http:`/`https:` URLs are opened — anything else (a relative path,
 * `file://`, `javascript:`, …) is rejected before it can reach the shell, so
 * an LLM-controlled string surfaced on a ToolCard can never make the app
 * open a non-web target. The opener receives the parsed URL's normalized
 * `href`, not the raw input, so odd-but-parseable forms (`http:example.com`,
 * backslashes) open exactly as a browser would resolve them. Errors are
 * logged but never thrown — a dead link should not crash the caller.
 *
 * @param url - the absolute http(s) URL to open.
 * @returns whether the URL was valid and the open call succeeded.
 */
export async function openExternal(url: string): Promise<boolean> {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return false;
  }
  try {
    await openUrl(parsed.href);
    return true;
  } catch (e) {
    console.error("failed to open external link:", parsed.href, e);
    return false;
  }
}
