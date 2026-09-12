// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT.
// See LICENSE in the repository root.

/**
 * The chat markdown link renderer (user report 2027-01-07: review-report
 * links in the chat failed to open — raw <a> anchors are dead under the
 * webview's CSP). Passed as the `a` component override to react-markdown
 * (Message.tsx): local file hrefs deep-link into the Files tab via
 * openFileInViewer (the same sanctioned path the write_review_report
 * tool-card chip uses), external http(s) hrefs open via openExternal
 * (the webfetch URL chip's shell-open pattern), and non-openable schemes
 * (mailto:, fragments) render as plain text with no dead affordance.
 */

import type { ReactNode } from "react";
import { normalizeLocalFileHref } from "../../lib/markdownLink";
import { openExternal } from "../../lib/openExternal";
import { openFileInViewer } from "../../lib/openFile";

/**
 * Route a markdown link click to the sanctioned opener. Extracted from
 * the component so the regression test can drive it without a DOM (the
 * node-env harness cannot fire events).
 */
export function openMarkdownTarget(href: string | undefined): void {
  if (!href) return;
  const trimmed = href.trim();
  if (trimmed === "") return;
  // External http(s) → the shell open (CSP blocks anchors; the webfetch
  // URL chip's established pattern).
  if (/^https?:\/\//i.test(trimmed)) {
    void openExternal(trimmed);
    return;
  }
  // Local file path → the Files-tab deep-link.
  const path = normalizeLocalFileHref(trimmed);
  if (path) openFileInViewer(path);
  // Anything else (mailto:, fragments, …) — no-op: no dead-link side effects.
}

/**
 * The `a` override for react-markdown: renders an openable affordance
 * whose click routes through openMarkdownTarget. Local file links
 * render as href-less button-styled anchors (no navigation affordance
 * the webview could dead-end on — only the sanctioned onClick); external
 * links keep their href; non-openable schemes render as plain text. The
 * author's markdown title (`[x](path "the title")`) is preserved,
 * suffixed with the affordance hint (review LOW 4).
 */
export function MarkdownLink({
  href,
  children,
  title,
}: {
  href?: string;
  children?: ReactNode;
  title?: string;
}) {
  if (!href) return <span>{children}</span>;
  const trimmed = href.trim();
  const external = /^https?:\/\//i.test(trimmed);
  const localPath = external ? null : normalizeLocalFileHref(trimmed);

  if (external) {
    return (
      <a
        href={trimmed}
        title={title ? `${title} (opens in your browser)` : "Opens in your browser"}
        className="cursor-pointer underline underline-offset-2"
        onClick={(e) => {
          e.preventDefault();
          openMarkdownTarget(trimmed);
        }}
      >
        {children}
      </a>
    );
  }
  if (localPath) {
    return (
      <a
        role="button"
        tabIndex={0}
        title={
          title
            ? `${title} (opens ${localPath} in the Files tab)`
            : `Opens ${localPath} in the Files tab`
        }
        className="cursor-pointer underline underline-offset-2"
        onClick={(e) => {
          e.preventDefault();
          openMarkdownTarget(trimmed);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            openMarkdownTarget(trimmed);
          }
        }}
      >
        {children}
      </a>
    );
  }
  // Non-openable (mailto:, fragments, …): plain text — no dead link.
  return <span>{children}</span>;
}
