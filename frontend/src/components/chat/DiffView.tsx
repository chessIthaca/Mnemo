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
 */

/** A single line in the unified diff. */
export interface DiffLine {
  type: "add" | "remove" | "context" | "meta";
  text: string;
}

/**
 * Product of `n * m` above which client LCS is skipped (Perf H4).
 * 2e6 ≈ ~1400×1400 lines — large enough for normal edits, small enough to
 * avoid multi-hundred-ms freezes and multi-GB DP tables.
 */
export const LCS_CELL_BUDGET = 2_000_000;

/** Max lines rendered in the budget-exceeded fallback (avoid painting 100k nodes). */
export const LCS_FALLBACK_MAX_LINES = 4_000;

/**
 * Parse a unified-diff string (as produced by Rust `similar` / `compute_diff`)
 * into colored `DiffLine`s. Header lines (`---`, `+++`, `@@`) become `meta`.
 */
export function parseUnifiedDiff(diffText: string): DiffLine[] {
  if (!diffText) return [];
  const lines = diffText.split("\n");
  // Drop a single trailing empty split from a final newline.
  if (lines.length > 0 && lines[lines.length - 1] === "") {
    lines.pop();
  }
  return lines.map((line) => {
    // Strict unified-diff headers (require the conventional trailing space /
    // @@ form) so content that merely begins with "---" after a +/- prefix is
    // not mis-classified when a producer omits the leading marker.
    if (
      line.startsWith("--- ") ||
      line.startsWith("+++ ") ||
      line.startsWith("@@ ") ||
      line === "---" ||
      line === "+++" ||
      /^@@/.test(line)
    ) {
      return { type: "meta" as const, text: line };
    }
    if (line.startsWith("+")) {
      return { type: "add" as const, text: line.slice(1) };
    }
    if (line.startsWith("-")) {
      return { type: "remove" as const, text: line.slice(1) };
    }
    // Context lines in unified diff start with a space; bare lines are rare.
    const text = line.startsWith(" ") ? line.slice(1) : line;
    return { type: "context" as const, text };
  });
}

/**
 * Compute a line-level diff using the LCS algorithm.
 *
 * Returns a list of `DiffLine`s in unified-diff order: removed lines appear
 * immediately before their added replacements, context lines are unchanged.
 *
 * When `oldLines.length * newLines.length` exceeds {@link LCS_CELL_BUDGET},
 * falls back to a linear side-by-side dump (all removes then all adds) so the
 * UI stays responsive. Prefer shipping a Rust `ApprovalPreview` so this path
 * is not hit for real file tools.
 */
export function computeDiff(oldText: string, newText: string): DiffLine[] {
  const oldLines = oldText.length === 0 ? [] : oldText.split("\n");
  const newLines = newText.length === 0 ? [] : newText.split("\n");
  const n = oldLines.length;
  const m = newLines.length;

  // Perf H4: refuse quadratic DP on huge inputs.
  if (n > 0 && m > 0 && n * m > LCS_CELL_BUDGET) {
    const result: DiffLine[] = [
      {
        type: "meta",
        text: `@@ diff truncated: ${n}×${m} lines exceeds client budget — showing capped remove/add @@`,
      },
    ];
    const half = Math.floor(LCS_FALLBACK_MAX_LINES / 2);
    const oldCap = Math.min(n, half);
    const newCap = Math.min(m, half);
    for (let k = 0; k < oldCap; k++) {
      result.push({ type: "remove", text: oldLines[k] });
    }
    if (n > oldCap) {
      result.push({
        type: "meta",
        text: `@@ … ${n - oldCap} more removed lines omitted … @@`,
      });
    }
    for (let k = 0; k < newCap; k++) {
      result.push({ type: "add", text: newLines[k] });
    }
    if (m > newCap) {
      result.push({
        type: "meta",
        text: `@@ … ${m - newCap} more added lines omitted … @@`,
      });
    }
    return result;
  }

  // Build the LCS length table.
  // dp[i][j] = length of LCS of oldLines[i..] and newLines[j..]
  const dp: number[][] = Array.from({ length: n + 1 }, () =>
    new Array(m + 1).fill(0)
  );
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      if (oldLines[i] === newLines[j]) {
        dp[i][j] = dp[i + 1][j + 1] + 1;
      } else {
        dp[i][j] = Math.max(dp[i + 1][j], dp[i][j + 1]);
      }
    }
  }

  // Walk the table to produce the diff.
  const result: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (oldLines[i] === newLines[j]) {
      result.push({ type: "context", text: oldLines[i] });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      result.push({ type: "remove", text: oldLines[i] });
      i++;
    } else {
      result.push({ type: "add", text: newLines[j] });
      j++;
    }
  }
  while (i < n) {
    result.push({ type: "remove", text: oldLines[i] });
    i++;
  }
  while (j < m) {
    result.push({ type: "add", text: newLines[j] });
    j++;
  }
  return result;
}

function renderDiffLines(lines: DiffLine[], maxHeightClass: string) {
  return (
    <div
      className={`overflow-auto rounded border border-border bg-bg-primary font-mono text-[0.75em] ${maxHeightClass}`}
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
      className={`overflow-auto rounded border border-border bg-bg-primary font-mono text-[0.75em] ${maxHeightClass}`}
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
