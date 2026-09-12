// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * In-memory prompt history for the InputBar's up/down navigation.
 *
 * A module-level singleton — survives InputBar remounts, per-session, no disk
 * persistence. Mirrors a shell's prompt history: push on send (dedupe
 * consecutive duplicates), navigate with up/down. Past the newest entry
 * restores the in-progress draft.
 *
 * Multiline-safe: the InputBar only navigates history when the cursor is on
 * the first line (up) or last line (down) — otherwise the textarea's default
 * cursor movement handles multiline editing. See `isOnFirstLine` /
 * `isOnLastLine`.
 */

/** The maximum number of prompts to retain (a ring buffer). */
const MAX_HISTORY = 100;

interface HistoryState {
  /** Oldest first, newest last. */
  entries: string[];
  /** The cursor position into `entries` for navigation. `-1` means "not
   * navigating" (the input shows the in-progress draft). `0` = newest. */
  cursor: number;
  /** The in-progress draft saved when navigation begins, restored when the
   * user moves past the newest entry. */
  draft: string;
}

const state: HistoryState = {
  entries: [],
  cursor: -1,
  draft: "",
};

/** Whether the cursor is on the first line of the textarea (no `\n` before it). */
export function isOnFirstLine(text: string, selectionStart: number): boolean {
  if (selectionStart === 0) return true;
  return !text.slice(0, selectionStart).includes("\n");
}

/** Whether the cursor is on the last line of the textarea (no `\n` after it). */
export function isOnLastLine(text: string, selectionStart: number): boolean {
  if (selectionStart >= text.length) return true;
  return !text.slice(selectionStart).includes("\n");
}

// ---------------------------------------------------------------------------
// Visual-line detection (multiline-safe for wrapped text)
// ---------------------------------------------------------------------------
//
// `isOnFirstLine`/`isOnLastLine` above only look for `\n`. A long prompt that
// wraps to several visual lines but contains no `\n` returns "first line" at
// ANY cursor position, so ArrowUp loads history instead of moving the cursor
// up a visual line. The functions below measure the textarea's actual visual
// layout via a cached off-screen mirror div, falling back to the `\n`-based
// helpers when layout is unavailable (jsdom / zero lineHeight).

/** A lazily-created, cached mirror div that replicates a textarea's layout. */
let mirrorDiv: HTMLDivElement | null = null;

/**
 * Get (creating + caching) the off-screen mirror div used to measure how a
 * string wraps at a given width + font. It is positioned off-screen (not
 * `display:none`, which would suppress layout) and re-styled to match the
 * source textarea each call (font/size/width/padding/etc. can change).
 */
function getMirror(ta: HTMLTextAreaElement): HTMLDivElement | null {
  if (typeof document === "undefined") return null;
  if (!mirrorDiv) {
    mirrorDiv = document.createElement("div");
    mirrorDiv.style.position = "absolute";
    mirrorDiv.style.visibility = "hidden";
    mirrorDiv.style.whiteSpace = "pre-wrap";
    mirrorDiv.style.wordWrap = "break-word";
    mirrorDiv.style.overflow = "hidden";
    mirrorDiv.style.top = "0";
    mirrorDiv.style.left = "-9999px";
  }
  // Copy the layout-affecting styles from the textarea so wrapping matches.
  const cs = window.getComputedStyle(ta);
  const copy: Array<keyof CSSStyleDeclaration> = [
    "fontFamily", "fontSize", "fontWeight", "lineHeight",
    "letterSpacing", "wordSpacing", "textIndent", "boxSizing",
    "paddingTop", "paddingRight", "paddingBottom", "paddingLeft",
    "borderTopWidth", "borderRightWidth", "borderBottomWidth", "borderLeftWidth",
    "width",
  ];
  for (const prop of copy) {
    // `as unknown as Record<string,string>` because CSSStyleDeclaration
    // indexing is stringly-typed; the cast is the documented escape hatch.
    (mirrorDiv.style as unknown as Record<string, string>)[prop as string] =
      (cs as unknown as Record<string, string>)[prop as string];
  }
  if (!mirrorDiv.parentNode && document.body) {
    document.body.appendChild(mirrorDiv);
  }
  return mirrorDiv;
}

/**
 * Measure how many visual lines a string occupies when laid out in the
 * textarea's style. Returns `null` when layout is unavailable (jsdom, zero
 * lineHeight, or a zero-height mirror — all indicate the measurement can't be
 * trusted and the caller should fall back to the `\n` heuristic).
 *
 * No trailing-newline sentinel: a `white-space: pre-wrap` div already renders
 * a trailing `\n` as an extra (empty) line, so the raw measurement is
 * correct. Appending a sentinel would overcount by one, which broke the
 * first-line check (see B1 in the review).
 */
function visualLineCount(ta: HTMLTextAreaElement, text: string): number | null {
  const mirror = getMirror(ta);
  if (!mirror) return null;
  mirror.textContent = text;
  const height = mirror.scrollHeight;
  const lineHeight = parseFloat(window.getComputedStyle(ta).lineHeight || "0");
  if (!lineHeight || !isFinite(lineHeight) || lineHeight <= 0 || height <= 0) {
    return null;
  }
  return Math.round(height / lineHeight);
}

/**
 * Convert a prefix's visual-line *count* (1-indexed: how many lines the text
 * before the cursor occupies) into the cursor's 0-indexed visual-line
 * *number*. A prefix that occupies 1 line means the cursor is on line 0; a
 * prefix that occupies 2 lines means the cursor is on line 1; etc. An empty
 * prefix (0 lines) means the cursor is on line 0. Extracted as a pure helper
 * so the 0-indexing logic is unit-testable without a DOM.
 */
export function cursorLineFromPrefixCount(prefixLineCount: number): number {
  return Math.max(0, prefixLineCount - 1);
}

/**
 * The visual line the cursor is on (0-indexed), or `null` when layout is
 * unavailable. Computed by measuring the text *up to* the cursor and
 * converting the line count to a 0-indexed line number. An empty prefix
 * (cursor at the start) yields a zero-height mirror → `visualLineCount`
 * returns `null` → the caller falls back to the `\n` heuristic, which is
 * correct for position 0 (always the first line).
 */
function cursorVisualLine(ta: HTMLTextAreaElement, selStart: number): number | null {
  const count = visualLineCount(ta, ta.value.slice(0, selStart));
  if (count === null) return null;
  return cursorLineFromPrefixCount(count);
}

/**
 * Whether the cursor is on the FIRST visual line of the textarea. Uses a
 * mirror-div layout measurement so wrapped text (no `\n`) is handled
 * correctly: ArrowUp only loads history when the cursor is on the top visual
 * line, otherwise the textarea's default cursor-up runs. Falls back to
 * [`isOnFirstLine`] when layout is unavailable.
 */
export function isOnFirstVisualLine(ta: HTMLTextAreaElement, selStart: number): boolean {
  const cursorLine = cursorVisualLine(ta, selStart);
  if (cursorLine === null) {
    return isOnFirstLine(ta.value, selStart);
  }
  return isFirstVisualLineFromCounts(cursorLine, visualLineCount(ta, ta.value) ?? 1);
}

/**
 * Whether the cursor is on the LAST visual line of the textarea. Uses a
 * mirror-div layout measurement so wrapped text (no `\n`) is handled
 * correctly: ArrowDown only loads history when the cursor is on the bottom
 * visual line. Falls back to [`isOnLastLine`] when layout is unavailable.
 */
export function isOnLastVisualLine(ta: HTMLTextAreaElement, selStart: number): boolean {
  const cursorLine = cursorVisualLine(ta, selStart);
  if (cursorLine === null) {
    return isOnLastLine(ta.value, selStart);
  }
  const total = visualLineCount(ta, ta.value) ?? 1;
  return isLastVisualLineFromCounts(cursorLine, total);
}

/**
 * Pure helper: whether a cursor on `cursorLine` (0-indexed) of `totalLines`
 * visual lines is on the first line. Extracted so the line-counting logic is
 * unit-testable without a real DOM layout. `totalLines` is intentionally
 * unused here (the first-line check depends only on the cursor's line), but
 * is kept in the signature for symmetry with [`isLastVisualLineFromCounts`].
 */
export function isFirstVisualLineFromCounts(cursorLine: number, _totalLines: number): boolean {
  return cursorLine <= 0;
}

/**
 * Pure helper: whether a cursor on `cursorLine` (0-indexed) of `totalLines`
 * visual lines is on the last line. Extracted so the line-counting logic is
 * unit-testable without a real DOM layout. `totalLines` is clamped to ≥1.
 */
export function isLastVisualLineFromCounts(cursorLine: number, totalLines: number): boolean {
  const total = Math.max(1, totalLines);
  return cursorLine >= total - 1;
}

/** Record a sent prompt (dedupe consecutive duplicates). Resets the cursor. */
export function pushPrompt(prompt: string): void {
  // Guard against empty / whitespace-only prompts (the InputBar already
  // guards on send, but this is defensive — a whitespace-only prompt should
  // never enter history).
  if (prompt.trim().length === 0) return;
  const entry = prompt;
  // Dedupe consecutive duplicates (don't push if it's the same as the last).
  if (state.entries.length > 0 && state.entries[state.entries.length - 1] === entry) {
    state.cursor = -1;
    state.draft = "";
    return;
  }
  state.entries.push(entry);
  // Ring buffer: drop the oldest when over capacity.
  if (state.entries.length > MAX_HISTORY) {
    state.entries.shift();
  }
  state.cursor = -1;
  state.draft = "";
}

/**
 * Navigate to the previous (older) prompt. Returns the prompt to show, or
 * `null` if there's no older entry (stay on the current). Call only when the
 * cursor is on the first line (multiline-safe — see isOnFirstLine).
 *
 * On the first Up, the current input is saved as the draft (restored when
 * navigating back past the newest entry).
 */
export function navigateUp(currentInput: string): string | null {
  if (state.entries.length === 0) return null;
  // First navigation: save the draft, point at the newest entry.
  if (state.cursor === -1) {
    state.draft = currentInput;
    state.cursor = 0;
    return state.entries[state.entries.length - 1 - state.cursor];
  }
  // Move older (increase the cursor offset from the newest).
  const next = state.cursor + 1;
  if (next >= state.entries.length) return null; // no older entry
  state.cursor = next;
  return state.entries[state.entries.length - 1 - state.cursor];
}

/**
 * Navigate to the next (newer) prompt. Returns the prompt to show, or the
 * saved draft when moving past the newest entry (restoring the in-progress
 * input), or `null` if already at the draft (no movement). Call only when the
 * cursor is on the last line (multiline-safe — see isOnLastLine).
 */
export function navigateDown(): string | null {
  if (state.cursor === -1) return null; // not navigating
  const prev = state.cursor - 1;
  if (prev < 0) {
    // Moved past the newest entry — restore the draft.
    state.cursor = -1;
    return state.draft;
  }
  state.cursor = prev;
  return state.entries[state.entries.length - 1 - state.cursor];
}

/** Reset navigation (e.g. when the user starts typing after navigating). */
export function resetNavigation(): void {
  state.cursor = -1;
  state.draft = "";
}

/** The current number of stored prompts (for tests). */
export function historyLength(): number {
  return state.entries.length;
}

/** Clear all history (for tests). */
export function clearHistory(): void {
  state.entries = [];
  state.cursor = -1;
  state.draft = "";
}
