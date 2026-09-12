// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Plan-step text helpers.
 *
 * A plan step is stored as `**Header** — body` (bold header + separator +
 * body). The Rust side (`extract_bold_header`) extracts the header for the
 * toolbar; this module is the frontend mirror for stripping the header to
 * get the body, used by the PlanProgress step rows.
 */

/**
 * Strip a leading bold header (and its separator) from a plan step's text,
 * returning the body. Plain text (no leading bold run) passes through
 * unchanged; a header-only step yields the empty string.
 *
 * Tolerant of marker runs longer than 2 — `****Header****` and the
 * asymmetric `****Header**` are the legacy double-wrapped form `create_plan`
 * used to write when a model passed an already-bold header (backlog
 * 2026-08-20, "****Verify builds/tests**** should be rendered bold"). The
 * regex mirrors the Rust extractor: a leading run of 2+ `*`, a non-greedy
 * header body (so a header containing a single `*` like `**src/*.rs**` still
 * matches), a closing run of 2+ `*`, then an optional separator (—/–/-/:)
 * and surrounding whitespace.
 */
export function stepBody(text: string): string {
  return text.replace(/^\*{2,}[\s\S]*?\*{2,}\s*[—–\-:]?\s*/, "");
}

/**
 * The single-line headline for a plan step as shown in the executing
 * popup: the bold header when present, else the text's first non-blank
 * line. Never the body — the popup stays one truncated line (the caller
 * CSS-truncates; the full headline rides the title attribute).
 */
export function stepHeadline(step: {
  header?: string | null;
  text: string;
}): string {
  if (step.header) return step.header;
  return step.text.split("\n").find((l) => l.trim().length > 0) ?? step.text;
}
