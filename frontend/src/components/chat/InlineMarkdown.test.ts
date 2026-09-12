// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { InlineMarkdown } from "./InlineMarkdown";

/**
 * InlineMarkdown contract — the single-line markdown surface for steer
 * bubbles and tool-card chips (backlog markdown plan, 2026-12). Unlike the
 * full MarkdownImpl pipeline this must NEVER emit block elements, and it
 * must be truncation-safe: an unterminated marker (a string cut mid-pair,
 * or a half-typed `**`) renders literally instead of swallowing text.
 */

/** Render text through the real InlineMarkdown component. */
function render(text: string): string {
  return renderToStaticMarkup(createElement(InlineMarkdown, { text }));
}

describe("InlineMarkdown (single-line surfaces)", () => {
  it("renders bold, italic, code, and strike", () => {
    expect(render("**bold**")).toBe("<strong>bold</strong>");
    expect(render("*italic*")).toBe("<em>italic</em>");
    expect(render("~~strike~~")).toBe("<del>strike</del>");
    expect(render("`code`")).toContain("<code");
    expect(render("`code`")).toContain(">code</code>");
  });

  it("renders multiple constructs mixed with plain text", () => {
    const html = render("Fix **the bug** in `src/main.rs`, ~~maybe~~");
    expect(html).toContain("<strong>the bug</strong>");
    expect(html).toContain(">src/main.rs</code>");
    expect(html).toContain("<del>maybe</del>");
    expect(html).toContain("Fix ");
  });

  it("styles code spans like chat inline code", () => {
    // Same classes as Message's inline-code renderer (theme-aware
    // .inline-code color) so chips/bubbles match chat styling.
    expect(render("`x`")).toContain('class="inline-code');
    expect(render("`x`")).toContain("bg-bg-tertiary");
  });

  it("recurses inside bold/italic (code inside bold)", () => {
    const html = render("**run `npm test` now**");
    expect(html).toContain("<strong>run <code");
    expect(html).toContain(">npm test</code> now</strong>");
  });

  it("renders code-span content literally (no nested markdown)", () => {
    const html = render("`**not bold**`");
    expect(html).toContain(">**not bold**</code>");
    expect(html).not.toContain("<strong>");
  });

  it("renders unterminated markers literally (truncation-safe)", () => {
    expect(render("**bold without close")).toBe("**bold without close");
    expect(render("run `npm test")).toBe("run `npm test");
    expect(render("*")).toBe("*");
    expect(render("ends with ~~")).toBe("ends with ~~");
  });

  it("renders empty marker pairs literally", () => {
    expect(render("****")).toBe("****");
    expect(render("``")).toBe("``");
  });

  it("passes plain text through unchanged", () => {
    expect(render("no markdown here")).toBe("no markdown here");
  });

  it("never emits block elements", () => {
    // Headings, lists, fences, tables in the input must stay literal text
    // — this component is only ever safe inside one-line chips/bubbles.
    const html = render("**b** `c` # not a heading\n- [ ] not a task");
    for (const block of ["<p>", "<ul>", "<li>", "<pre>", "<h1>", "<table>", "<div>"]) {
      expect(html).not.toContain(block);
    }
    expect(html).toContain("# not a heading");
    expect(html).toContain("- [ ] not a task");
  });
});
