// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Minimal INLINE markdown renderer for single-line, truncation-safe
 * surfaces (InputBar steer bubbles, backlog_add tool-card chips).
 *
 * Unlike the full pipeline ([`Markdown`](./Markdown) → react-markdown +
 * remark-gfm + rehype-highlight) this never emits block elements — no
 * paragraphs, lists, headings, or code fences — so it is safe inside
 * `truncate` spans and one-line chips. It recognizes exactly four inline
 * constructs: `**bold**`, `*italic*`, `` `code` `` (styled like chat's
 * inline code), and `~~strike~~`. Bold/italic/strike recurse (code inside
 * bold works); code spans render their content literally. Unterminated or
 * empty markers render as literal text, so a truncated or half-typed
 * string never swallows surrounding words. This is deliberately NOT a
 * CommonMark parser: intraword `*` and adjacent `*`/`**` mixes are out of
 * scope — use the full [`Markdown`](./Markdown) pipeline for those.
 */
import type { ReactNode } from "react";

/** One inline construct: its literal delimiter and how its inner text renders. */
type MarkerSpec = {
  /** The literal opening/closing delimiter (`**`, `~~`, `*`, `` ` ``). */
  marker: string;
  /** Whether the inner text is itself tokenized (code spans render literally). */
  recursive: boolean;
  /** Render the matched inner text as this element. */
  render: (inner: ReactNode, key: number) => ReactNode;
};

// Order matters: `**` must be scanned before `*` so the longer delimiter
// wins when both match at the same position (the scan keeps the earliest
// match; on ties the spec seen first — the longer marker — is kept).
const MARKER_SPECS: MarkerSpec[] = [
  {
    marker: "**",
    recursive: true,
    render: (inner, key) => <strong key={key}>{inner}</strong>,
  },
  {
    marker: "~~",
    recursive: true,
    render: (inner, key) => <del key={key}>{inner}</del>,
  },
  {
    marker: "*",
    recursive: true,
    render: (inner, key) => <em key={key}>{inner}</em>,
  },
  {
    marker: "`",
    recursive: false,
    // Same classes as Message's inline-code renderer so chips and bubbles
    // match chat styling (.inline-code is the theme-aware color in
    // globals.css).
    render: (inner, key) => (
      <code
        key={key}
        className="inline-code rounded bg-bg-tertiary px-1.5 py-0.5 text-xs"
      >
        {inner}
      </code>
    ),
  },
];

/**
 * Tokenize `text` into inline-markdown nodes (plain strings + styled
 * elements). A delimiter without a closing partner — or with empty inner
 * text — renders literally and scanning resumes right after it, which is
 * what makes truncated input safe.
 */
function tokenizeInline(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let plain = "";
  let i = 0;
  let key = 0;
  while (i < text.length) {
    // Earliest marker at or after i; ties keep the first (longest) spec.
    let spec: MarkerSpec | null = null;
    let pos = -1;
    for (const s of MARKER_SPECS) {
      const p = text.indexOf(s.marker, i);
      if (p !== -1 && (spec === null || p < pos)) {
        spec = s;
        pos = p;
      }
    }
    if (spec === null) {
      plain += text.slice(i);
      break;
    }
    plain += text.slice(i, pos);
    const close = text.indexOf(spec.marker, pos + spec.marker.length);
    const inner =
      close === -1 ? "" : text.slice(pos + spec.marker.length, close);
    if (close === -1 || inner === "") {
      // Unterminated or empty — the delimiter is literal text.
      plain += spec.marker;
      i = pos + spec.marker.length;
      continue;
    }
    if (plain !== "") {
      nodes.push(plain);
      plain = "";
    }
    nodes.push(
      spec.render(spec.recursive ? tokenizeInline(inner) : inner, key++),
    );
    i = close + spec.marker.length;
  }
  if (plain !== "") nodes.push(plain);
  return nodes;
}

/**
 * Render `text` as inline markdown — `**bold**`, `*italic*`, `` `code` ``,
 * `~~strike~~` — emitting no block elements ever, for single-line surfaces
 * (steer bubbles, tool-card chips). Unterminated markers render literally.
 */
export function InlineMarkdown({ text }: { text: string }) {
  return <>{tokenizeInline(text)}</>;
}

export default InlineMarkdown;
