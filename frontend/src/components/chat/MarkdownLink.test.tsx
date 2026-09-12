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
import { openExternal } from "../../lib/openExternal";
import { MarkdownLink, openMarkdownTarget } from "./MarkdownLink";
import messageSource from "./Message.tsx?raw";

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
      rightPanelVisible: true,
      pendingFileOpen: null,
    });
    vi.mocked(openExternal).mockReset();
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

  it("routes external http(s) hrefs through openExternal (the shell open)", () => {
    openMarkdownTarget("https://example.com/docs");
    expect(vi.mocked(openExternal)).toHaveBeenCalledWith(
      "https://example.com/docs",
    );
    expect(useAgentStore.getState().pendingFileOpen).toBeNull();
  });

  it("ignores non-openable schemes (no dead-link side effects)", () => {
    openMarkdownTarget("mailto:a@b.c");
    expect(useAgentStore.getState().pendingFileOpen).toBeNull();
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
    expect(external).toContain('title="the site (opens in your browser)"');
  });
});

describe("Message.tsx wiring (source contract)", () => {
  it("the chat's Markdown components override `a` with MarkdownLink", () => {
    expect(messageSource).toContain("a: MarkdownLink");
  });
});
