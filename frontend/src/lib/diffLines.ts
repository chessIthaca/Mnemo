// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The React-free diff core shared by the chat DiffView and the Mnemo2 Svelte
 * tool rows: one line type, one unified-diff parser, and the size-guarded
 * client LCS.
 *
 * Extracted from `components/chat/DiffView.tsx` (verbatim move) so the Svelte
 * workspace can import it too — Mnemo2's tsconfig sets no `--jsx`, so pulling
 * the helpers from the React component fails the build (`'--jsx' is not set`,
 * probe 2026-11-02); a pure `.ts` module has no such dependency. DiffView
 * re-exports everything below, so its existing importers are unchanged.
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