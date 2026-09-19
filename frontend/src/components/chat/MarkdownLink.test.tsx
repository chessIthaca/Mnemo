// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Chat markdown file links (user report 2027-01-07, plan 28bc06a2's
 * review): clicking a review-report link in the chat failed to open the
 * file — markdown links rendered as raw <a href> anchors, and the app's
 * production CSP (default-src 'self') blocks anchor navigation in the
 * Tauri webview, so the click could never open anything.
 *
 * These tests pin the fix's contract: local file hrefs route through
 * the sanctioned openFileInViewer (the Files-tab deep-link — the same
 * path the write_review_report tool-card chip uses), external http(s)
 * hrefs through openExternal (the webfetch URL chip's shell-open
 * pattern), and non-openable schemes render as plain text with no dead
 * affordance. The click routing is driven through the REAL
 * requestFileOpen store action (no store mock) — the node-env harness
 * cannot fire DOM events, so the router function is the tested seam and
 * the Message.tsx wiring is pinned as a source contract.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useAgentStore } from "../../hooks/useAgentStore";
import { normalizeLocalFileHref } from "../../lib/markdownLink";

// The shell open never runs in tests — the factory mock replaces the
// module (so @tauri-apps/plugin-shell is never even loaded in node).
vi.mock("../../lib/openExternal", () => ({ openExternal: vi.fn() }));
// The router's platform probe is stubbed; the real module is spread so every
// other ./tauri export still resolves for the store's import graph.
vi.mock("../../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../lib/tauri")>()),
  browserWebviewSupported: vi.fn(),
}));
import { openExternal } from "../../lib/openExternal";
import { browserWebviewSupported } from "../../lib/tauri";
import { MarkdownLink, openMarkdownTarget } from "./MarkdownLink";
import messageSource from "./Message.tsx?raw";
import markdownImplSource from "./MarkdownImpl.tsx?raw";

/**
 * The router awaits the (mocked) platform probe before touching the store, so
 * a test that asserts the store must let that microtask chain settle.
 */
const flushRouter = (): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, 0));

describe("normalizeLocalFileHref", () => {
  it("passes canonical project-relative paths through", () => {
    expect(normalizeLocalFileHref(".coding/reviews/2026-09-07-x.md")).toBe(
      ".coding/reviews/2026-09-07-x.md",
    );
    expect(normalizeLocalFileHref("src/main.rs")).toBe("src/main.rs");
  });

  it("strips ./ and / prefixes", () => {
    expect(normalizeLocalFileHref("./src/main.rs")).toBe("src/main.rs");
    expect(normalizeLocalFileHref("/.coding/plans/p1.md")).toBe(
      ".coding/plans/p1.md",
    );
    expect(normalizeLocalFileHref("././.coding/knowledge/x.md")).toBe(
      ".coding/knowledge/x.md",
    );
  });

  it("converts backslash separators to forward slashes", () => {
    expect(normalizeLocalFileHref(".coding\\reviews\\x.md")).toBe(
      ".coding/reviews/x.md",
    );
  });

  it("percent-decodes escaped characters", () => {
    expect(normalizeLocalFileHref("src/a%20b.md")).toBe("src/a b.md");
  });

  it("strips a trailing fragment (GitHub-style line anchors)", () => {
    expect(normalizeLocalFileHref("src/foo.ts#L10")).toBe("src/foo.ts");
  });

  it("keeps an encoded literal hash in the filename (raw # is the separator)", () => {
    // A file named "a#b.md" must be percent-encoded in markdown; the
    // raw-# fragment strip must not eat it (review LOW 1).
    expect(normalizeLocalFileHref("src/a%23b.md")).toBe("src/a#b.md");
  });

  it("strips a query string (GitHub-style URLs pasted with a query)", () => {
    // Review LOW 2: a query must not leak into the opened path.
    expect(normalizeLocalFileHref("src/foo.ts?x=1")).toBe("src/foo.ts");
    expect(normalizeLocalFileHref("src/foo.ts?x=1#frag")).toBe("src/foo.ts");
  });

  it("rejects external URLs, schemes, fragments, and empty hrefs", () => {
    expect(normalizeLocalFileHref("https://example.com/x")).toBeNull();
    expect(normalizeLocalFileHref("http://example.com")).toBeNull();
    expect(normalizeLocalFileHref("mailto:a@b.c")).toBeNull();
    expect(normalizeLocalFileHref("#fragment")).toBeNull();
    expect(normalizeLocalFileHref("")).toBeNull();
    // Windows absolute paths look like scheme forms (C:) — not openable
    // through the project-relative viewer path.
    expect(normalizeLocalFileHref("C:/abs/path.md")).toBeNull();
    // UNC forms (backslash server paths) — rejected after the
    // backslash→slash conversion, not degraded to a bogus relative
    // path (review LOW 3).
    expect(normalizeLocalFileHref("\\\\server\\share\\x.md")).toBeNull();
  });
});

describe("openMarkdownTarget — the click router", () => {
  beforeEach(() => {
    useAgentStore.setState({
      rightPanelTab: "plan",
      disabledTabs: [],
      rightPanelVisible: false,
      pendingFileOpen: null,
      pendingBrowserUrl: null,
    });
    vi.mocked(openExternal).mockReset();
    vi.mocked(openExternal).mockResolvedValue(true);
    vi.mocked(browserWebviewSupported).mockReset();
    vi.mocked(browserWebviewSupported).mockResolvedValue(true);
  });

  /**
   * The regression test body — named (rather than an inline `it`
   * callback) so the code graph and the plan's regression-test record
   * can point at it (user report 2027-01-07: review-report links in
   * the chat failed to open).
   */
  function localFileHrefRoutesThroughRequestFileOpen(): void {
    openMarkdownTarget(".coding/reviews/2026-09-07-x.md");
    const s = useAgentStore.getState();
    expect(s.pendingFileOpen).toEqual({
      path: ".coding/reviews/2026-09-07-x.md",
      line: null,
    });
    expect(s.rightPanelTab).toBe("files");
    expect(vi.mocked(openExternal)).not.toHaveBeenCalled();
  }

  it(
    "routes a local file href through the sanctioned requestFileOpen path",
    localFileHrefRoutesThroughRequestFileOpen,
  );

  it("normalizes the href before opening (backslashes, ./, percent-encoding)", () => {
    openMarkdownTarget("./.coding\\plans\\my%20plan.md");
    expect(useAgentStore.getState().pendingFileOpen).toEqual({
      path: ".coding/plans/my plan.md",
      line: null,
    });
  });

  it("routes external http(s) hrefs into the app's BROWSER TAB (no OS-browser launch)", async () => {
    // User request 2027-01-16: the link must load in the right-panel Browser
    // tab, and the OS browser must NOT be launched.
    openMarkdownTarget("https://example.com/docs");
    await flushRouter();
    const s = useAgentStore.getState();
    expect(s.pendingBrowserUrl).toBe("https://example.com/docs");
    expect(s.rightPanelTab).toBe("browser");
    expect(s.rightPanelVisible).toBe(true);
    expect(s.pendingFileOpen).toBeNull();
    expect(vi.mocked(openExternal)).not.toHaveBeenCalled();
  });

  it("keeps the OS default browser for the ctrl/cmd-click escape hatch", async () => {
    openMarkdownTarget("https://example.com/docs", { osBrowser: true });
    await flushRouter();
    expect(vi.mocked(openExternal)).toHaveBeenCalledWith(
      "https://example.com/docs",
    );
    expect(useAgentStore.getState().pendingBrowserUrl).toBeNull();
  });

  it("falls back to the OS browser where the Browser tab cannot exist", async () => {
    // macOS/Linux: no child webview, so the link goes to the OS browser.
    vi.mocked(browserWebviewSupported).mockResolvedValue(false);
    openMarkdownTarget("https://example.com/docs");
    await flushRouter();
    expect(vi.mocked(openExternal)).toHaveBeenCalledWith(
      "https://example.com/docs",
    );
    expect(useAgentStore.getState().pendingBrowserUrl).toBeNull();
  });

  it("ignores non-openable schemes (no dead-link side effects)", async () => {
    // Awaited: the http(s) path settles in a microtask (the router awaits the
    // platform probe first), so without the flush a wrongly-routed mailto
    // would still read as "nothing happened" below.
    openMarkdownTarget("mailto:a@b.c");
    await flushRouter();
    const s = useAgentStore.getState();
    expect(s.pendingFileOpen).toBeNull();
    // The TAB channel too (round-2 review): a wrongly-accepted scheme routes to
    // the app's Browser tab — not to the OS browser — so the two assertions
    // below would stay green while the click had already revealed the panel.
    expect(s.pendingBrowserUrl).toBeNull();
    expect(s.rightPanelTab).toBe("plan");
    expect(vi.mocked(openExternal)).not.toHaveBeenCalled();
  });

  it("does NOT widen the schemes chat can open (data:/file: stay inert)", async () => {
    // The Browser tab's normalize_url also accepts data:/file: — the router
    // must not hand those to it just because the tab could load them. Awaited
    // for the same reason as above: the store write happens only after the
    // router's `await browserWebviewSupported()`, so a widened allow-list
    // would otherwise be invisible to these assertions.
    openMarkdownTarget("data:text/html,<b>x</b>");
    openMarkdownTarget("file:///C:/notes.html");
    await flushRouter();
    const s = useAgentStore.getState();
    expect(s.pendingBrowserUrl).toBeNull();
    expect(s.pendingFileOpen).toBeNull();
    expect(vi.mocked(openExternal)).not.toHaveBeenCalled();
  });
});

describe("MarkdownLink — the rendered affordance", () => {
  it("renders a local file link as a button-styled anchor with NO href", () => {
    const html = renderToStaticMarkup(
      <MarkdownLink href=".coding/reviews/2026-09-07-x.md">report</MarkdownLink>,
    );
    expect(html).toContain("report");
    expect(html).toContain('role="button"');
    expect(html).toContain("cursor-pointer");
    expect(html).not.toContain("href=");
  });

  it("renders an external link as an anchor that keeps its href", () => {
    const html = renderToStaticMarkup(
      <MarkdownLink href="https://example.com">site</MarkdownLink>,
    );
    expect(html).toContain('href="https://example.com"');
    expect(html).toContain("cursor-pointer");
  });

  it("renders non-openable schemes as plain text (no dead link)", () => {
    const html = renderToStaticMarkup(
      <MarkdownLink href="mailto:a@b.c">mail me</MarkdownLink>,
    );
    expect(html).toContain("mail me");
    expect(html).not.toContain("href=");
    expect(html).not.toContain('role="button"');
  });

  it("merges the author's markdown title with the affordance hint", () => {
    // Review LOW 4: `[x](path "the title")` — the author's title is
    // preserved, suffixed with the open hint.
    const html = renderToStaticMarkup(
      <MarkdownLink href=".coding/reviews/x.md" title="the report">
        report
      </MarkdownLink>,
    );
    expect(html).toContain(
      'title="the report (opens .coding/reviews/x.md in the Files tab)"',
    );
    const external = renderToStaticMarkup(
      <MarkdownLink href="https://example.com" title="the site">
        site
      </MarkdownLink>,
    );
    expect(external).toContain(
      'title="the site (opens in the Browser tab — ctrl-click for your browser)"',
    );
  });
});

describe("Message.tsx wiring (source contract)", () => {
  it("the chat's Markdown components override `a` with MarkdownLink", () => {
    expect(messageSource).toContain("a: MarkdownLink");
  });

  it("the web_fetch URL chip routes through openChatLink, never a bare shell open", () => {
    // User request 2027-01-16: the chip must load the page in the app's own
    // Browser tab (with the router's OS-browser fallback), not always shell
    // out — so no direct openExternal call may survive in this file.
    expect(messageSource).toContain("openChatLink(url");
    expect(messageSource).not.toContain("openExternal");
  });
});

describe("MarkdownImpl default link override (source contract)", () => {
  it("every markdown surface gets `a: MarkdownLink` by default — no raw anchors anywhere", () => {
    // User report 2027-01-24, backlog 3f838ea1: plan goals, backlog bodies and
    // the editor preview render <Markdown> with no components override, so
    // their links were raw <a href> anchors whose default navigation
    // replaced the whole app UI. The override now defaults INSIDE the single
    // ReactMarkdown renderer (a caller's own `a` still wins — spread after).
    expect(markdownImplSource).toContain("a: MarkdownLink, ...components");
  });
});
