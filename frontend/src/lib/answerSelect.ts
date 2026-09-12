// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Pure helpers for resolving a pending `ask_user` question from the InputBar.
//
// The user can answer a pending question by typing `1)`, `2)`, … (digit(s)
// followed by `)`) in the standard command box. This module parses that input
// and maps it to a choice index or the "Let's talk about it" freeform choice,
// keeping the logic pure + unit-testable (independent of React/the store).

/**
 * The result of parsing a numeric answer from the InputBar.
 * - `choice` with a 0-indexed option index (a real option was selected).
 * - `freeform` (the "Let's talk about it" number was selected — the InputBar
 *   should switch into freeform-answer mode).
 * - `null` when the input is not a numeric answer (or is out of range).
 */
export type NumericAnswer =
  | { kind: "choice"; index: number }
  | { kind: "freeform" }
  | null;

/**
 * Parse a numeric answer from the InputBar text.
 *
 * Matches `N)` where N is one or more digits (e.g. `1)`, `12)`). Maps N to:
 * - `1..options.length` → `choice` with index `N-1`.
 * - `options.length + 1` → `freeform` (the "Let's talk about it" choice).
 * - anything else (out of range, or not the `N)` syntax) → `null` (fall
 *   through to a normal send).
 *
 * The input is trimmed first, so `  2)  ` still matches. A bare `2` (no `)`)
 * does NOT match — it would otherwise hijack ordinary numeric prompts.
 *
 * @param text The raw InputBar text.
 * @param optionCount The number of real options in the pending question.
 */
export function parseNumericAnswer(
  text: string,
  optionCount: number,
): NumericAnswer {
  const trimmed = text.trim();
  const match = /^(\d+)\)$/.exec(trimmed);
  if (!match) return null;
  const n = parseInt(match[1], 10);
  if (Number.isNaN(n) || n < 1) return null;
  if (n <= optionCount) {
    return { kind: "choice", index: n - 1 };
  }
  if (n === optionCount + 1) {
    return { kind: "freeform" };
  }
  // Out of range — ignore (fall through to a normal send).
  return null;
}
