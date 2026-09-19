// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The chat-link router (user request 2027-01-16, backlog fc812ffe): an http(s)
 * link clicked in the chat must load in the app's OWN Browser tab instead of
 * shelling out to the OS default browser, with the OS browser kept for the
 * explicit click modifiers and for platforms where the child webview cannot
 * exist.
 *
 * Node-env suite: `openExternal` and the platform probe
 * (`browserWebviewSupported`) are mocked at their module boundaries while the
 * REAL store runs, so the routing decision AND its store side effect are both
 * exercised. BrowserView's consume side has no DOM harness — it is pinned as a
 * source contract at the bottom of this file.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./openExternal", () => ({ openExternal: vi.fn() }));
// Spread the real module so every other ./tauri export still resolves for the
// store's import graph; only the platform probe is replaced.
vi.mock("./tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./tauri")>()),
  browserWebviewSupported: vi.fn(),
}));

import { useAgentStore } from "../hooks/useAgentStore";
import browserViewSource from "../components/views/BrowserView.tsx?raw";
import { openChatLink } from "./openChatLink";
import { openExternal } from "./openExternal";
import { browserWebviewSupported } from "./tauri";

/** A known right-panel state, with no deep-link request left over. */
function resetState(): void {
  useAgentStore.setState({
    rightPanelTab: "plan",
    disabledTabs: [],
    rightPanelVisible: true,
    pendingFileOpen: null,
    pendingBrowserUrl: null,
  });
}

describe("openChatLink", () => {
  beforeEach(() => {
    resetState();
    vi.mocked(openExternal).mockReset();
    vi.mocked(openExternal).mockResolvedValue(true);
    vi.mocked(browserWebviewSupported).mockReset();
    vi.mocked(browserWebviewSupported).mockResolvedValue(true);
  });

  it("loads an http(s) link in the Browser tab, with no OS-browser launch", async () => {
    await openChatLink("https://example.com/docs");
    const s = useAgentStore.getState();
    expect(s.pendingBrowserUrl).toBe("https://example.com/docs");
    expect(s.rightPanelTab).toBe("browser");
    expect(s.rightPanelVisible).toBe(true);
    expect(vi.mocked(openExternal)).not.toHaveBeenCalled();
  });

  it("enables a DISABLED Browser tab and reveals the panel for the link", async () => {
    // The common case for a fresh session: the Browser tab starts disabled and
    // the right panel may be hidden — the deep-link must still land there.
    useAgentStore.setState({
      rightPanelTab: "plan",
      disabledTabs: ["browser"],
      rightPanelVisible: false,
    });
    await openChatLink("https://example.com/docs");
    const s = useAgentStore.getState();
    expect(s.disabledTabs).not.toContain("browser");
    expect(s.rightPanelTab).toBe("browser");
    expect(s.rightPanelVisible).toBe(true);
  });

  it("falls back to the OS browser where the child webview cannot exist", async () => {
    // macOS/Linux: the Browser tab would only show its unsupported-platform
    // panel, so the pre-existing OS-browser behavior is kept there.
    vi.mocked(browserWebviewSupported).mockResolvedValue(false);
    await openChatLink("https://example.com/docs");
    expect(vi.mocked(openExternal)).toHaveBeenCalledWith("https://example.com/docs");
    expect(useAgentStore.getState().pendingBrowserUrl).toBeNull();
    expect(useAgentStore.getState().rightPanelTab).toBe("plan");
  });

  it("honors the explicit OS-browser modifier (ctrl/cmd/middle click)", async () => {
    await openChatLink("https://example.com/docs", { osBrowser: true });
    expect(vi.mocked(openExternal)).toHaveBeenCalledWith("https://example.com/docs");
    expect(useAgentStore.getState().pendingBrowserUrl).toBeNull();
    // The modifier wins outright: the platform probe is not even consulted.
    expect(vi.mocked(browserWebviewSupported)).not.toHaveBeenCalled();
  });

  it("ignores every non-http(s) target instead of inventing an opener", async () => {
    // mailto:/ftp: are not openable (the pre-existing rule), relative paths and
    // fragments are not URLs at all, and data:/file: must NOT be widened into
    // the tab just because normalize_url would accept them.
    for (const target of [
      "mailto:a@b.c",
      "ftp://host/file",
      "src/main.rs",
      "#fragment",
      "",
      "data:text/html,<b>x</b>",
      "file:///C:/notes.html",
      "javascript:alert(1)",
    ]) {
      await openChatLink(target);
    }
    expect(vi.mocked(openExternal)).not.toHaveBeenCalled();
    expect(useAgentStore.getState().pendingBrowserUrl).toBeNull();
    expect(useAgentStore.getState().rightPanelTab).toBe("plan");
  });

  it("treats a failing platform probe as supported (the tab degrades visibly)", async () => {
    // A transient IPC failure must not silently relaunch the OS browser; the
    // tab path reports its own error instead. Mirrors BrowserView's optimistic
    // mount probe.
    vi.mocked(browserWebviewSupported).mockRejectedValue(new Error("ipc down"));
    await openChatLink("https://example.com/docs");
    expect(useAgentStore.getState().pendingBrowserUrl).toBe("https://example.com/docs");
    expect(vi.mocked(openExternal)).not.toHaveBeenCalled();
  });

  it("hands on the NORMALIZED href, like openExternal does", async () => {
    await openChatLink("HTTPS://Example.COM/docs");
    expect(useAgentStore.getState().pendingBrowserUrl).toBe("https://example.com/docs");
  });
});

/**
 * Source contract for BrowserView's consume side (the node env cannot mount the
 * component). The pending URL must be read from the store and fed through the
 * view's OWN rect-aware ensure + navigate path, then cleared so it is
 * single-shot. A regression to a fire-and-forget navigate would leave the child
 * webview never created (or moved to a wrong rect), and this pin fails loudly.
 */
describe("BrowserView consumes the pending browser URL", () => {
  it("reads pendingBrowserUrl, loads it through the rect-aware path, and clears it", () => {
    expect(browserViewSource).toContain("loadIntoChild(pendingBrowserUrl)");
    expect(browserViewSource).toContain("clearPendingBrowserUrl");
    expect(browserViewSource).toContain("browserWebviewEnsure");
    expect(browserViewSource).toContain("browserWebviewNavigate");
    // The "moved to a wrong rect" half of the claim: the rect must come from
    // THIS view's placeholder (both strings occur only inside loadIntoChild).
    expect(browserViewSource).toContain("areaRef.current");
    expect(browserViewSource).toContain("getBoundingClientRect");
  });

  it("syncs the URL box from the child webview's current URL (store.browserUrl)", () => {
    // Backlog 3f838ea1: agent-steered CDP navigations and in-child link
    // clicks never route through loadIntoChild — the view must subscribe to
    // the store's browserUrl (written by the module-scope
    // browser://url-changed listener) and mirror it into the URL box +
    // loadedUrl, so the box shows the current URL on change AND on mount
    // (the store tracks it even while the tab is hidden).
    expect(browserViewSource).toContain("s.browserUrl");
    expect(browserViewSource).toContain("setUrl(browserUrl)");
    expect(browserViewSource).toContain("setLoadedUrl(browserUrl)");
  });
});
