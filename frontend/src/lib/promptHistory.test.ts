// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect, beforeEach } from "vitest";
import {
  pushPrompt,
  navigateUp,
  navigateDown,
  isOnFirstLine,
  isOnLastLine,
  isOnFirstVisualLine,
  isOnLastVisualLine,
  isFirstVisualLineFromCounts,
  isLastVisualLineFromCounts,
  cursorLineFromPrefixCount,
  resetNavigation,
  historyLength,
  clearHistory,
} from "./promptHistory";

describe("promptHistory", () => {
  beforeEach(() => {
    clearHistory();
  });

  describe("first/last-line detection", () => {
    it("single-line text: cursor is on both first and last line", () => {
      const text = "hello world";
      expect(isOnFirstLine(text, 0)).toBe(true);
      expect(isOnFirstLine(text, 5)).toBe(true);
      expect(isOnFirstLine(text, text.length)).toBe(true);
      expect(isOnLastLine(text, 0)).toBe(true);
      expect(isOnLastLine(text, 5)).toBe(true);
      expect(isOnLastLine(text, text.length)).toBe(true);
    });

    it("multiline: first line only when no newline before cursor", () => {
      const text = "line1\nline2\nline3";
      // Cursor in line1 (positions 0–5, before the first \n).
      expect(isOnFirstLine(text, 0)).toBe(true);
      expect(isOnFirstLine(text, 5)).toBe(true);
      // Cursor at/after the first \n is NOT on the first line.
      expect(isOnFirstLine(text, 6)).toBe(false);
      expect(isOnFirstLine(text, 10)).toBe(false);
    });

    it("multiline: last line only when no newline after cursor", () => {
      const text = "line1\nline2\nline3";
      // Cursor in line3 (positions 12–17, after the last \n).
      expect(isOnLastLine(text, 12)).toBe(true);
      expect(isOnLastLine(text, text.length)).toBe(true);
      // Cursor in line1/line2 is NOT on the last line (a \n follows).
      expect(isOnLastLine(text, 0)).toBe(false);
      expect(isOnLastLine(text, 6)).toBe(false);
      expect(isOnLastLine(text, 11)).toBe(false);
    });

    it("empty text: both first and last line", () => {
      expect(isOnFirstLine("", 0)).toBe(true);
      expect(isOnLastLine("", 0)).toBe(true);
    });
  });

  describe("visual-line pure helpers", () => {
    it("cursor on line 0 of 3 → first=true, last=false", () => {
      expect(isFirstVisualLineFromCounts(0, 3)).toBe(true);
      expect(isLastVisualLineFromCounts(0, 3)).toBe(false);
    });

    it("cursor on line 2 of 3 → last=true, first=false", () => {
      expect(isLastVisualLineFromCounts(2, 3)).toBe(true);
      expect(isFirstVisualLineFromCounts(2, 3)).toBe(false);
    });

    it("cursor on line 1 of 3 → neither first nor last", () => {
      expect(isFirstVisualLineFromCounts(1, 3)).toBe(false);
      expect(isLastVisualLineFromCounts(1, 3)).toBe(false);
    });

    it("single line (total=1) → both first and last", () => {
      expect(isFirstVisualLineFromCounts(0, 1)).toBe(true);
      expect(isLastVisualLineFromCounts(0, 1)).toBe(true);
    });

    it("clamps totalLines to ≥1 (defensive)", () => {
      // A zero/negative total (shouldn't happen, but be defensive) is treated
      // as 1 line, so the cursor is on both the first and last line.
      expect(isFirstVisualLineFromCounts(0, 0)).toBe(true);
      expect(isLastVisualLineFromCounts(0, -1)).toBe(true);
    });
  });

  describe("cursorLineFromPrefixCount (0-indexing — regression guard for B1)", () => {
    // B1: cursorVisualLine used to return a 1-indexed line COUNT (the "\n"
    // sentinel inflated it to ≥1), so isFirstVisualLineFromCounts's `<= 0`
    // check was never true in a real browser → ArrowUp history navigation was
    // broken. cursorLineFromPrefixCount converts a prefix's line count into
    // the cursor's 0-indexed line number; these tests pin the conversion.
    it("a 1-line prefix → cursor on line 0 (first line)", () => {
      expect(cursorLineFromPrefixCount(1)).toBe(0);
    });

    it("a 2-line prefix → cursor on line 1", () => {
      expect(cursorLineFromPrefixCount(2)).toBe(1);
    });

    it("a 3-line prefix → cursor on line 2", () => {
      expect(cursorLineFromPrefixCount(3)).toBe(2);
    });

    it("a 0-line prefix (empty) → cursor on line 0 (clamped)", () => {
      expect(cursorLineFromPrefixCount(0)).toBe(0);
    });

    it("combined with isFirstVisualLineFromCounts: 1-line prefix → first=true", () => {
      // The single-line "hello world" case from B1: prefix occupies 1 line →
      // cursor line 0 → first visual line → ArrowUp loads history. ✓
      expect(isFirstVisualLineFromCounts(cursorLineFromPrefixCount(1), 1)).toBe(true);
    });

    it("combined: 2-line prefix of a 3-line prompt → neither first nor last", () => {
      // Cursor on the middle visual line of a wrapped 3-line prompt.
      expect(isFirstVisualLineFromCounts(cursorLineFromPrefixCount(2), 3)).toBe(false);
      expect(isLastVisualLineFromCounts(cursorLineFromPrefixCount(2), 3)).toBe(false);
    });
  });

  describe("visual-line detection fallback", () => {
    // The vitest suite runs in a node environment (no DOM), so the mirror-div
    // measurement is unavailable and the visual-line helpers fall back to the
    // `\n`-based heuristic. This is the same path a real browser hits when
    // lineHeight is 0/NaN. We pass a duck-typed object — when layout is
    // unavailable, only `ta.value` is read (no DOM methods are called).
    it("falls back to \\n-based detection when layout is unavailable", () => {
      const ta = { value: "line1\nline2\nline3" } as unknown as HTMLTextAreaElement;
      // Cursor in line1 (before the first \n) → first visual line.
      expect(isOnFirstVisualLine(ta, 0)).toBe(true);
      expect(isOnFirstVisualLine(ta, 5)).toBe(true);
      // Cursor in line2 (after the first \n) → NOT the first visual line.
      expect(isOnFirstVisualLine(ta, 6)).toBe(false);
      // Cursor in line3 (after the last \n) → last visual line.
      expect(isOnLastVisualLine(ta, 12)).toBe(true);
      // Cursor in line1 → NOT the last visual line (a \n follows).
      expect(isOnLastVisualLine(ta, 0)).toBe(false);
    });
  });

  describe("history navigation", () => {
    it("push, up, up, down, down — navigates + restores draft", () => {
      pushPrompt("first");
      pushPrompt("second");
      pushPrompt("third");
      expect(historyLength()).toBe(3);

      // Start navigating from a draft "drafting..." — Up saves it.
      let shown = navigateUp("drafting...");
      expect(shown).toBe("third"); // newest first

      shown = navigateUp("third");
      expect(shown).toBe("second");

      shown = navigateUp("second");
      expect(shown).toBe("first");

      // Past the oldest — no movement (returns null).
      shown = navigateUp("first");
      expect(shown).toBeNull();

      // Down moves back toward newer.
      shown = navigateDown();
      expect(shown).toBe("second");

      shown = navigateDown();
      expect(shown).toBe("third");

      // One more Down — past the newest, restores the draft.
      shown = navigateDown();
      expect(shown).toBe("drafting...");

      // One more Down — already at draft, no movement.
      shown = navigateDown();
      expect(shown).toBeNull();
    });

    it("dedupes consecutive duplicates", () => {
      pushPrompt("same");
      pushPrompt("same");
      pushPrompt("same");
      expect(historyLength()).toBe(1);
    });

    it("does not dedupe non-consecutive duplicates", () => {
      pushPrompt("a");
      pushPrompt("b");
      pushPrompt("a");
      expect(historyLength()).toBe(3);
    });

    it("up with empty history returns null", () => {
      expect(navigateUp("drafting")).toBeNull();
    });

    it("down when not navigating returns null", () => {
      pushPrompt("x");
      expect(navigateDown()).toBeNull();
    });

    it("resetNavigation clears the cursor + draft", () => {
      pushPrompt("only");
      navigateUp("draft");
      resetNavigation();
      // After reset, Down does nothing (not navigating).
      expect(navigateDown()).toBeNull();
      // And Up starts fresh from the newest, saving a new draft.
      const shown = navigateUp("new draft");
      expect(shown).toBe("only");
    });

    it("ring buffer caps at MAX_HISTORY (100)", () => {
      for (let i = 0; i < 150; i++) {
        pushPrompt(`prompt-${i}`);
      }
      expect(historyLength()).toBe(100);
      // The oldest 50 were dropped; the newest is prompt-149.
      const shown = navigateUp("draft");
      expect(shown).toBe("prompt-149");
    });
  });
});
