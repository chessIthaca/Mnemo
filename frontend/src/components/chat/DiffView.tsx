// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * A shared line-by-line diff renderer used by both the inline approval prompt
 * and the right-panel DiffViewer.
 *
 * Prefer rendering a Rust-side unified diff (`UnifiedDiffView`) when
 * `ApprovalPreview` is available. The client LCS path remains as a fallback
 * for older events / non-file tools and is size-guarded so large edits cannot
 * allocate an O(n·m) DP table.
 *
 * The parsing/LCS core (`DiffLine`, `parseUnifiedDiff`, `computeDiff`, the
 * LCS budgets) lives in `lib/diffLines.ts` — extracted verbatim so the
 * React-free helpers can be shared with Mnemo2's Svelte workspace (its
 * tsconfig sets no `--jsx`, so importing this .tsx from there fails the
 * build) — and is re-exported below so existing importers keep working
 * unchanged.
 */

import { type DiffLine, computeDiff, parseUnifiedDiff } from "../../lib/diffLines";

export { computeDiff, parseUnifiedDiff } from "../../lib/diffLines";
export { LCS_CELL_BUDGET, LCS_FALLBACK_MAX_LINES } from "../../lib/diffLines";
export type { DiffLine } from "../../lib/diffLines";

function renderDiffLines(lines: DiffLine[], maxHeightClass: string) {
  return (
    <div
      className={`overflow-auto rounded-sm border border-border bg-bg-primary font-mono text-[0.75em] ${maxHeightClass}`}
    >
      {lines.length === 0 ? (
        <div className="px-2 py-1 text-slate-500">(no changes)</div>
      ) : (
        lines.map((line, idx) => {
          if (line.type === "meta") {
            return (
              <div
                key={idx}
                className="whitespace-pre px-2 py-0.5 text-slate-500"
              >
                {line.text}
              </div>
            );
          }
          const prefix =
            line.type === "add" ? "+" : line.type === "remove" ? "-" : " ";
          const color =
            line.type === "add"
              ? "text-green-300 bg-green-950/30"
              : line.type === "remove"
                ? "text-red-300 bg-red-950/30"
                : "text-slate-400";
          return (
            <div key={idx} className={`whitespace-pre px-2 py-0.5 ${color}`}>
              <span className="select-none opacity-60">{prefix} </span>
              {line.text}
            </div>
          );
        })
      )}
    </div>
  );
}

interface DiffViewProps {
  oldText: string;
  newText: string;
  /** Optional max height (Tailwind class) for the scroll container. */
  maxHeightClass?: string;
}

/** Client LCS diff between two full texts (args fallback path). */
export function DiffView({
  oldText,
  newText,
  maxHeightClass = "max-h-64",
}: DiffViewProps) {
  const lines = computeDiff(oldText, newText);
  return renderDiffLines(lines, maxHeightClass);
}

interface UnifiedDiffViewProps {
  /** Unified-diff text from Rust `ApprovalPreview::Diff`. */
  diffText: string;
  maxHeightClass?: string;
}

/** Render a precomputed unified diff (preferred path when preview is set). */
export function UnifiedDiffView({
  diffText,
  maxHeightClass = "max-h-64",
}: UnifiedDiffViewProps) {
  const lines = parseUnifiedDiff(diffText);
  return renderDiffLines(lines, maxHeightClass);
}

interface NewFileViewProps {
  content: string;
  maxHeightClass?: string;
}

/** Green add-only view for new-file / overwrite content previews. */
export function NewFileView({
  content,
  maxHeightClass = "max-h-64",
}: NewFileViewProps) {
  const lines = content.length === 0 ? [""] : content.split("\n");
  return (
    <div
      className={`overflow-auto rounded-sm border border-border bg-bg-primary font-mono text-[0.75em] ${maxHeightClass}`}
    >
      {lines.map((line, idx) => (
        <div
          key={idx}
          className="whitespace-pre bg-green-950/20 px-2 py-0.5 text-green-300"
        >
          <span className="select-none opacity-60">+ </span>
          {line}
        </div>
      ))}
    </div>
  );
}
