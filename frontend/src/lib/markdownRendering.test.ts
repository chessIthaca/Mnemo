// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import remarkBreaks from "remark-breaks";
import { MarkdownImpl } from "../components/chat/MarkdownImpl";
import markdownSource from "../components/chat/Markdown.tsx?raw";

/**
 * The markdown renderer contract — an executable audit of what formatting
 * the shared stack (react-markdown + remark-gfm + rehype-highlight, i.e.
 * micromark/CommonMark + GFM) renders correctly. This covers the surfaces
 * that render markdown as HTML: chat messages, the FileViewer, and plan
 * goals. (Plan-step HEADERS are intentionally plain text + font-semibold,
 * not markdown — their bold comes from CSS.)
 *
 * Added with backlog 2026-08-20 ("****Verify builds/tests**** should be
 * rendered bold"): the reported bug turned out to be in the plan-step
 * data/display path (double-wrapped headers + a brittle extractor), NOT in
 * this renderer — `****x****` is nested `<strong>` per CommonMark, which
 * this test now proves and pins. If a formatting feature ever regresses
 * (a remark/rehype upgrade changing behavior), the failing case here names
 * it immediately.
 */

/** Render markdown through the REAL shared renderer implementation. */
function render(md: string): string {
  return renderToStaticMarkup(createElement(MarkdownImpl, null, md));
}

describe("markdown renderer contract (MarkdownImpl)", () => {
  it("renders nested-bold markers (****x****) as strong — the reported bug shape", () => {
    const html = render("****Verify builds/tests****");
    expect(html).toContain("<strong>");
    expect(html).toContain("Verify builds/tests");
    expect(html).not.toContain("****");
  });

  it("renders basic emphasis", () => {
    expect(render("**bold**")).toContain("<strong>");
    expect(render("*italic*")).toContain("<em>");
    expect(render("`code`")).toContain("<code>");
  });

  it("renders GFM strikethrough", () => {
    expect(render("~~strike this~~")).toContain("<del>");
  });

  it("renders GFM tables", () => {
    const html = render("| a | b |\n|---|---|\n| 1 | 2 |");
    expect(html).toContain("<table>");
    expect(html).toContain("<th>a</th>");
  });

  it("renders GFM task lists", () => {
    const html = render("- [x] done thing");
    expect(html).toContain("checkbox");
    expect(html).toContain("checked");
  });

  it("highlights fenced code blocks via hljs", () => {
    const html = render("```rust\nfn main() {}\n```");
    expect(html).toContain("hljs");
    expect(html).toContain("language-rust");
  });

  it("autolinks bare URLs (GFM autolink literal)", () => {
    expect(render("see https://example.com/x now")).toContain(
      '<a href="https://example.com/x"',
    );
  });

  it("renders hard line breaks (two trailing spaces)", () => {
    expect(render("line one  \nline two")).toContain("<br/>");
  });

  it("appends caller remarkPlugins after remarkGfm (remark-breaks: single \\n → <br>)", () => {
    // The backlog card body passes remark-breaks so a single newline
    // renders as a line break (2026-08-21 regression) while
    // the default CommonMark behavior of chat/plan/FileViewer is unchanged.
    const html = renderToStaticMarkup(
      createElement(MarkdownImpl, {
        remarkPlugins: [remarkBreaks],
        children: "**bold**\nline two",
      }),
    );
    expect(html).toContain("<strong>bold</strong>");
    expect(html).toContain("<br/>");
  });

  it("keeps default single-newline behavior without remarkPlugins", () => {
    // CommonMark: a lone \n inside a paragraph is a soft break rendered as
    // a space (no <br/>) — chat/plan/FileViewer rely on this.
    expect(render("line one\nline two")).not.toContain("<br/>");
  });
});

describe("Markdown fallback (lazy-load window) source contract", () => {
  it("keeps single newlines visible in the fallback div (whitespace-pre-wrap)", () => {
    // During the lazy-load window the raw markdown renders in the fallback
    // div; without pre-wrap, single newlines collapse to spaces — the
    // 2026-08-21 regression in transient form (review finding on plan
    // af0a7ea2). Once loaded, the pipeline owns line breaks (remark-breaks
    // on the backlog surfaces).
    expect(markdownSource).toContain(
      'className="markdown-fallback whitespace-pre-wrap break-words"',
    );
  });
});
