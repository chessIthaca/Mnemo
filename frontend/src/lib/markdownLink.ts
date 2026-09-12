// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT.
// See LICENSE in the repository root.

/**
 * Local-file href normalization for chat markdown links (user report
 * 2027-01-07: review-report links rendered as raw <a> anchors that the
 * webview's CSP could never open). Pure, node-testable — the component
 * side lives in components/chat/MarkdownLink.tsx.
 */

/** Matches scheme forms like "https:", "mailto:", "C:" (Windows drives). */
const SCHEME_RE = /^[a-zA-Z][a-zA-Z0-9+.-]*:/;

/**
 * Normalize a markdown link href into the project-relative,
 * forward-slash path form the sanctioned file opener
 * (`openFileInViewer` → the Files-tab deep-link) expects, or `null` when
 * the href is not a local file path (external URLs, schemes, pure
 * fragments, empty).
 *
 * Normalizations: strip the fragment and query on the RAW href (a raw
 * "#" / "?" is the separator — an encoded literal "%23" / "%3F"
 * survives decoding as a real character), percent-decode (raw on
 * malformed sequences), backslashes → forward slashes, strip leading
 * "./" and "/" prefixes. Traversal segments ("..") are left for the
 * backend path sandbox to reject — never munged here.
 */
export function normalizeLocalFileHref(href: string): string | null {
  const trimmed = href.trim();
  if (trimmed === "") return null;

  // Strip the fragment, then the query, on the RAW href (before
  // decoding): a raw "#" / "?" is the separator, so an encoded literal
  // ("%23" / "%3F") survives decoding as a real character (review
  // LOW 1/2).
  let raw = trimmed;
  const hashIdx = raw.indexOf("#");
  if (hashIdx !== -1) raw = raw.slice(0, hashIdx);
  const queryIdx = raw.indexOf("?");
  if (queryIdx !== -1) raw = raw.slice(0, queryIdx);
  if (raw === "") return null;

  // Percent-decode (markdown hrefs escape spaces etc.); fall back to
  // the raw href on malformed sequences.
  let decoded = raw;
  try {
    decoded = decodeURIComponent(raw);
  } catch {
    // keep the raw href
  }

  // Scheme forms (https:, mailto:, C:) are not local file paths. Checked
  // on the DECODED form so an encoded scheme ("%68ttps:") cannot smuggle
  // past.
  if (SCHEME_RE.test(decoded)) return null;

  // Backslash separators → forward slashes (Windows-written paths)…
  let path = decoded.replace(/\\/g, "/");
  // …then reject protocol-relative / UNC forms ("//server/share/…") —
  // AFTER the conversion, so a backslash UNC form ("\\server\share\…")
  // is caught too instead of degrading to a bogus relative path (review
  // LOW 3).
  if (path.startsWith("//")) return null;

  // Strip leading "./" sequences and "/" prefixes (root-relative).
  while (path.startsWith("./")) path = path.slice(2);
  path = path.replace(/^\/+/, "");
  if (path === "") return null;
  return path;
}
