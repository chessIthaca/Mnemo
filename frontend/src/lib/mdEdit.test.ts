// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Unit tests for the Markdown editor's pure text-transform helpers
 * (see `mdEdit.ts`). Node environment — no DOM needed by design.
 */
import { describe, expect, it } from "vitest";
import {
  hasUnsavedChanges,
  insertBlock,
  normalizeForSave,
  setLinePrefix,
  wrapSelection,
} from "./mdEdit";

describe("wrapSelection", () => {
  it("wraps an existing selection and selects the inner text", () => {
    const r = wrapSelection("hello world", 0, 5, "**", "**");
    expect(r.text).toBe("**hello** world");
    expect(r.selStart).toBe(2);
    expect(r.selEnd).toBe(7);
  });

  it("inserts and selects the placeholder when the cursor is collapsed", () => {
    const r = wrapSelection("abc", 1, 1, "`", "`", "code");
    expect(r.text).toBe("a`code`bc");
    expect(r.selStart).toBe(2);
    expect(r.selEnd).toBe(6);
  });
});

describe("setLinePrefix", () => {
  it("prefixes a single selected line", () => {
    const r = setLinePrefix("Title\nbody", 0, 5, "## ");
    expect(r.text).toBe("## Title\nbody");
    expect(r.selStart).toBe(0);
    expect(r.selEnd).toBe(8);
  });

  it("prefixes every selected line and expands partial first/last lines", () => {
    const r = setLinePrefix("ab\ncd\nef", 1, 4, "- "); // selects "b\nc" (mid-line to mid-line)
    expect(r.text).toBe("- ab\n- cd\nef");
    expect(r.selStart).toBe(0);
    expect(r.selEnd).toBe(9);
  });

  it("applies to the cursor's own line when collapsed", () => {
    const r = setLinePrefix("x\ny", 2, 2, "- ");
    expect(r.text).toBe("x\n- y");
    expect(r.selStart).toBe(2);
    expect(r.selEnd).toBe(5);
  });

  it("does not pull in the following line when the selection ends on a newline", () => {
    const r = setLinePrefix("a\nb\nc", 0, 2, "- "); // selects "a\n"
    expect(r.text).toBe("- a\nb\nc");
    expect(r.selEnd).toBe(3);
  });

  it("preserves CRLF separators", () => {
    const r = setLinePrefix("a\r\nb", 0, 5, "- "); // whole doc
    expect(r.text).toBe("- a\r\n- b");
  });

  it("CRLF selection ending after a newline keeps the \\r with its newline", () => {
    const r = setLinePrefix("a\r\nb", 0, 3, "- "); // selects "a\r\n"
    expect(r.text).toBe("- a\r\nb");
  });
});

describe("insertBlock", () => {
  it("inserts a block mid-document with blank-line separation", () => {
    const r = insertBlock("# T\nbody", 4, 4, "```\n\n```");
    expect(r.text).toBe("# T\n\n```\n\n```\n\nbody");
    expect(r.selStart).toBe(5 + "```\n\n```".length);
    expect(r.selEnd).toBe(r.selStart);
  });

  it("adds no leading gap at the very top of the document", () => {
    const r = insertBlock("a", 0, 0, "X");
    expect(r.text).toBe("X\n\na");
  });

  it("ends the file with a single trailing newline at EOF", () => {
    const r = insertBlock("a", 1, 1, "X");
    expect(r.text).toBe("a\n\nX\n");
  });

  it("collapses existing blank lines instead of stacking gaps", () => {
    const r = insertBlock("a\n\n\nb", 4, 4, "X"); // cursor on the empty second line
    expect(r.text).toBe("a\n\nX\n\nb");
  });

  it("uses CRLF gaps in a CRLF document", () => {
    const r = insertBlock("a\r\nb", 1, 1, "X");
    expect(r.text).toBe("a\r\n\r\nX\r\n\r\nb");
  });

  it("styles the block body with the document's CRLF endings (C2 regression)", () => {
    // An LF-typed block (e.g. the toolbar's code fence) inserted into a CRLF
    // document must come out fully CRLF — not LF fences between CRLF gaps.
    const r = insertBlock("a\r\nb", 1, 1, "```\n\n```");
    expect(r.text).toBe("a\r\n\r\n```\r\n\r\n```\r\n\r\nb");
    expect(r.text.includes("```\n")).toBe(false);
  });
});

describe("normalizeForSave", () => {
  it("converts LF-typed edits back to CRLF for a CRLF file", () => {
    expect(normalizeForSave("a\r\nb\r\n", "a\nb\nc\n")).toBe("a\r\nb\r\nc\r\n");
  });

  it("returns LF-file edits verbatim", () => {
    expect(normalizeForSave("a\nb", "a\nb\r\nextra")).toBe("a\nb\r\nextra");
  });
});

describe("hasUnsavedChanges", () => {
  it("detects real edits", () => {
    expect(hasUnsavedChanges("a\nb", "a\n\nc")).toBe(true);
  });

  it("identical content is clean", () => {
    expect(hasUnsavedChanges("a\nb", "a\nb")).toBe(false);
  });

  it("a lone trailing-newline difference is not a change", () => {
    expect(hasUnsavedChanges("a", "a\n")).toBe(false);
    expect(hasUnsavedChanges("a\n", "a")).toBe(false);
    expect(hasUnsavedChanges("a", "a\r\n")).toBe(false);
  });
});
