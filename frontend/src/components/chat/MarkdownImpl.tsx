// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The actual markdown renderer implementation.
 *
 * This file statically imports `react-markdown` + `remark-gfm` +
 * `rehype-highlight` — the heavy markdown/highlight stack. It is reached ONLY
 * via dynamic `import()` from [`Markdown`](./Markdown), so the whole stack
 * lands in a separate Vite chunk instead of the main bundle.
 */
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";

import { MarkdownLink } from "./MarkdownLink";

/**
 * Render `children` (a markdown string) as formatted markdown.
 *
 * `remarkGfm` enables GitHub-flavored extensions (tables, strikethrough, task
 * lists). `rehypeHighlight` applies syntax highlighting to fenced code blocks.
 * `components` optionally overrides element renderers (e.g. a custom `code`
 * renderer) — passed through from the caller, so caller-side components stay
 * in the main bundle. The `a` override defaults to [`MarkdownLink`] for EVERY
 * surface (user report 2027-01-24, backlog 3f838ea1): a raw `<a href>` anchor
 * runs the WebView's default navigation and replaces the whole app UI, so
 * links must route through the sanctioned openers (http(s) → the Browser tab
 * via openChatLink; local files → the Files tab) wherever markdown renders —
 * chat messages, plan goals, backlog bodies, the editor preview. A caller's
 * own `a` (spread after the default) still wins. `remarkPlugins` optionally
 * appends caller-supplied remark plugins AFTER the shared `remarkGfm` — used
 * by the backlog surfaces to add `remark-breaks` (single newlines render as
 * line breaks, preserving the 2026-08-21 newline-display regression) without
 * changing the default CommonMark behavior of chat messages, plan goals, or
 * the FileViewer.
 */
export function MarkdownImpl({
  children,
  components,
  remarkPlugins: extraRemarkPlugins,
}: {
  children: string;
  components?: React.ComponentProps<typeof ReactMarkdown>["components"];
  remarkPlugins?: React.ComponentProps<typeof ReactMarkdown>["remarkPlugins"];
}) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm, ...(extraRemarkPlugins ?? [])]}
      rehypePlugins={[rehypeHighlight]}
      components={{ a: MarkdownLink, ...components }}
    >
      {children}
    </ReactMarkdown>
  );
}

export default MarkdownImpl;
