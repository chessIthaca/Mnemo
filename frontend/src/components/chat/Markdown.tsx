// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Lazy-loaded Markdown renderer.
 *
 * Wraps the heavy markdown/highlight stack (`react-markdown` + `remark-gfm` +
 * `rehype-highlight`) behind a dynamic import so it lands in a separate Vite
 * chunk instead of the main bundle. The first render shows a plain-text
 * fallback (the raw markdown) until the chunk loads; subsequent renders use
 * the cached module. Used by Message, FileViewer, and PlanProgress.
 */
import { Suspense, lazy, type ReactNode } from "react";

const MarkdownImpl = lazy(() => import("./MarkdownImpl"));

/** A minimal inline fallback: render the raw markdown as preformatted text.
 *  `whitespace-pre-wrap` keeps single newlines rendering as line breaks
 *  during the lazy-load window (2026-08-21 regression, transient form)
 *  until the pipeline loads; `break-words` matches the rendered path. */
function MarkdownFallback({ children }: { children: ReactNode }) {
  return <div className="markdown-fallback whitespace-pre-wrap break-words">{children}</div>;
}

/**
 * Render `children` (a markdown string) as formatted markdown.
 *
 * `remarkGfm` enables GitHub-flavored extensions (tables, strikethrough, task
 * lists). `rehypeHighlight` applies syntax highlighting to fenced code blocks.
 * Both are lazy-loaded on first use via [`MarkdownImpl`](./MarkdownImpl).
 * `components` optionally overrides element renderers (passed through to the
 * lazy impl). `remarkPlugins` optionally appends caller-supplied remark
 * plugins after the shared `remarkGfm` (passed through to the lazy impl) —
 * the backlog surfaces use it for `remark-breaks` so single newlines keep
 * rendering as line breaks (2026-08-21 regression) without changing the
 * default pipeline behavior for chat/plan/FileViewer.
 */
export function Markdown({
  children,
  components,
  remarkPlugins,
}: {
  children: string;
  components?: React.ComponentProps<typeof MarkdownImpl>["components"];
  remarkPlugins?: React.ComponentProps<typeof MarkdownImpl>["remarkPlugins"];
}) {
  return (
    <Suspense fallback={<MarkdownFallback>{children}</MarkdownFallback>}>
      <MarkdownImpl components={components} remarkPlugins={remarkPlugins}>
        {children}
      </MarkdownImpl>
    </Suspense>
  );
}
