// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useRef, useState } from "react";
import { Globe, ExternalLink, RotateCw, X } from "lucide-react";
import {
  browserNormalizeUrl,
  browserWebviewEnsure,
  browserWebviewNavigate,
  browserWebviewReload,
  browserWebviewStop,
  browserWebviewSetTabVisible,
  browserWebviewSupported,
  errMsg,
} from "../../lib/tauri";
import { useBrowserRect } from "../../hooks/useBrowserRect";

/**
 * Right-panel Browser view — a native child WebView2 embedded in the "main"
 * Tauri window (replaces the old iframe). The human drives it directly
 * (clicks, types, plays) AND the agent inspects/controls it via the `browser_*`
 * tools (which attach to the child webview over its CDP debug port).
 *
 * A child WebView2 is a separate OS-level instance — it is never "framed," so
 * sites that refuse framing via X-Frame-Options / CSP frame-ancestors (Google,
 * YouTube) load normally. The URL bar normalizes input through the same
 * `normalize_url` choke point the headless `browser_navigate` uses, so a bare
 * hostname like `google.com` loads `https://google.com` and a local HTML file
 * can be opened as `file:///C:/path/page.html` (pure HTML debugging).
 *
 * This component renders a placeholder `<div>` (the child webview renders
 * above this rect as a native HWND). It reports the rect to the backend on
 * mount + resize (via `useBrowserRect`) so Rust can position/size the child,
 * and on Open it ensures the child exists + navigates it. Stop and Reload
 * buttons halt the in-flight load and re-issue the current page (both safe
 * no-ops before the first Open). The child is shown
 * when the tab is active and hidden when a modal overlay opens (see
 * `useBrowserOverlay`).
 *
 * On platforms without WebView2 (macOS/Linux) the tab is unavailable: the
 * backend's `browser_webview_supported` reports false on mount and this
 * component renders an explanatory red panel instead of the URL bar — the
 * agent's `browser_*` tools are not registered there either.
 */
export function BrowserView() {
  const [url, setUrl] = useState("");
  const [loadedUrl, setLoadedUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Optimistic true — Windows is the primary target, so the tab renders
  // immediately there without a flicker; other platforms flip to the
  // unsupported panel once the backend answers on mount.
  const [supported, setSupported] = useState(true);
  const areaRef = useRef<HTMLDivElement>(null);

  // Ask the backend once whether the native child webview exists on this
  // platform (Windows-only: WebView2 + CDP; see ipc/browser_webview.rs).
  useEffect(() => {
    let alive = true;
    void browserWebviewSupported()
      .then((ok) => {
        if (alive) setSupported(ok);
      })
      // A rejection (IPC failure / older backend without the command) is
      // deliberately swallowed: `supported` stays optimistically true, which
      // renders the normal tab — never worse than the pre-check behavior
      // (review F4).
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  // Report the placeholder rect to the backend so the child webview tracks it.
  // Skipped entirely on platforms that have no child webview.
  useBrowserRect(areaRef, supported);

  // Tell the backend the Browser tab is active on mount, inactive on unmount.
  // A no-op on unsupported platforms (no child webview was ever created).
  useEffect(() => {
    if (!supported) return;
    void browserWebviewSetTabVisible(true);
    return () => {
      void browserWebviewSetTabVisible(false);
    };
  }, [supported]);

  /** Commit the typed URL to the child webview (called on Open or Enter). */
  async function openUrl() {
    const target = url.trim();
    if (!target) return;
    try {
      const normalized = await browserNormalizeUrl(target);
      // Ensure the child webview exists (created on first call) + navigate it.
      // The rect is reported continuously by useBrowserRect; pass the current
      // rect so the child is positioned correctly on creation.
      const el = areaRef.current;
      if (el) {
        const r = el.getBoundingClientRect();
        const dpr = window.devicePixelRatio || 1;
        await browserWebviewEnsure(
          Math.round(r.x * dpr),
          Math.round(r.y * dpr),
          Math.round(r.width * dpr),
          Math.round(r.height * dpr),
          normalized,
        );
      }
      await browserWebviewNavigate(normalized);
      setLoadedUrl(normalized);
      setError(null);
    } catch (e) {
      setError(errMsg(e));
    }
  }

  /** Halt the child webview's in-flight page load (no-op when nothing loads). */
  async function stopLoad() {
    try {
      await browserWebviewStop();
      setError(null);
    } catch (e) {
      setError(errMsg(e));
    }
  }

  /** Hard-reload the child webview's current page (bypasses cache). */
  async function reloadPage() {
    try {
      await browserWebviewReload();
      setError(null);
    } catch (e) {
      setError(errMsg(e));
    }
  }

  // Unsupported platform (no WebView2): an explanatory red panel replaces the
  // URL bar + placeholder entirely. All hooks above still run — this only
  // changes what renders, never hook ordering.
  if (!supported) {
    return (
      <div className="flex h-full flex-col gap-2 p-2 text-[0.875em]">
        <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2 rounded border border-red-900 bg-red-950/40 p-4 text-center">
          <h3 className="text-[1.05em] font-semibold text-red-300">
            Browser tab unavailable on this platform
          </h3>
          <p className="max-w-prose text-[0.85em] leading-relaxed text-red-300/90">
            The embedded browser is a native Microsoft WebView2 instance, and
            the agent&apos;s <code>browser_*</code> tools attach to it through its
            CDP debug protocol — both exist only on Windows. On this platform
            the tab is disabled and those tools are not registered. The
            agent&apos;s headless <code>offscreen_browser_*</code> tools remain available.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col gap-2 p-2 text-[0.875em]">
      {/* URL bar */}
      <div className="flex items-center gap-1.5">
        <input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void openUrl();
          }}
          placeholder="e.g. google.com, http://localhost:3000, or file:///C:/page.html"
          className="min-w-0 flex-1 rounded border border-border bg-bg-tertiary px-2 py-1 text-[0.85em] text-slate-200 placeholder:text-slate-600 focus:border-cyan-500 focus:outline-none"
        />
        <button
          onClick={() => void stopLoad()}
          title="Stop loading the page"
          aria-label="Stop loading the page"
          className="flex items-center rounded border border-border bg-bg-tertiary px-2 py-1 text-[0.85em] text-slate-300 hover:text-cyan-400"
        >
          <X className="h-3.5 w-3.5" />
        </button>
        <button
          onClick={() => void reloadPage()}
          title="Reload the current page (bypasses cache)"
          aria-label="Reload the current page (bypasses cache)"
          className="flex items-center rounded border border-border bg-bg-tertiary px-2 py-1 text-[0.85em] text-slate-300 hover:text-cyan-400"
        >
          <RotateCw className="h-3.5 w-3.5" />
        </button>
        <button
          onClick={() => void openUrl()}
          title="Load the URL in the browser"
          className="flex items-center gap-1 rounded border border-border bg-bg-tertiary px-2 py-1 text-[0.85em] text-slate-300 hover:text-cyan-400"
        >
          <ExternalLink className="h-3.5 w-3.5" />
          Open
        </button>
      </div>

      {error && (
        <div className="rounded border border-red-900 bg-red-950/40 px-2 py-1 text-[0.8em] text-red-300">
          {error}
        </div>
      )}

      {/* Placeholder for the native child WebView2 — the child renders above
          this rect as a separate HWND. The human plays here; the agent
          inspects/controls it via the browser_* tools (CDP attach to the child). */}
      <div className="flex min-h-0 flex-1 overflow-hidden rounded border border-border bg-bg-tertiary">
        <div ref={areaRef} className="h-full w-full">
          {!loadedUrl && (
            <div className="flex h-full flex-col items-center justify-center text-slate-500">
              <Globe className="mb-2 h-8 w-8 opacity-50" />
              <p>No page loaded.</p>
              <p className="text-[0.75em]">
                Enter a URL above (e.g. google.com, http://localhost:3000, or
                file:///C:/page.html).
              </p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
