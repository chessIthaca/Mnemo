// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Pure text-transform helpers for the FileViewer's Markdown editor.
 *
 * Every helper takes a textarea snapshot (the full `text` plus selection
 * bounds) and returns the NEW text + selection, so the component stays a thin
 * shell (apply the result, restore the selection) and all editing logic is
 * unit-testable in the node vitest environment with no DOM.
 *
 * CRLF is respected throughout: line-oriented helpers preserve the exact line
 * separators of the touched region, and block insertion picks the document's
 * dominant ending style for the blank-line gaps it inserts.
 */

/** The result of a text transform: the new text plus the new selection. */
export interface TextSel {
  /** The full document text after the transformation. */
  text: string;
  /** The new selection start (a caret when equal to `selEnd`). */
  selStart: number;
  /** The new selection end. */
  selEnd: number;
}

/**
 * Wrap the selection with `before`/`after` markers (e.g. `**`…`**` for bold,
 * `` ` ``…`` ` `` for inline code). A collapsed cursor inserts the
 * `placeholder` between the markers and selects it, ready to type over.
 */
export function wrapSelection(
  text: string,
  selStart: number,
  selEnd: number,
  before: string,
  after: string,
  placeholder = "text",
): TextSel {
  const selected = text.slice(selStart, selEnd);
  const inner = selected === "" ? placeholder : selected;
  const next =
    text.slice(0, selStart) + before + inner + after + text.slice(selEnd);
  const innerStart = selStart + before.length;
  return { text: next, selStart: innerStart, selEnd: innerStart + inner.length };
}

/**
 * Prefix every (partially) selected line with `prefix` — `"# "`/`"## "`/`"### "`
 * for headers, `"- "` for bullets, `"1. "` for numbered lists. A collapsed
 * cursor applies to the line it sits on. Existing line separators (LF or
 * CRLF) are preserved exactly; the new selection covers the prefixed lines.
 */
export function setLinePrefix(
  text: string,
  selStart: number,
  selEnd: number,
  prefix: string,
): TextSel {
  // Start of the first touched line (char after the preceding newline).
  const ls = text.lastIndexOf("\n", Math.max(0, selStart - 1)) + 1;
  // End of the last touched line. When the selection ends right AFTER a
  // newline, that newline closes the last selected line — don't pull in the
  // following line (a collapsed cursor at a line start must not fire this).
  const endsAtNewline = selEnd > selStart && text[selEnd - 1] === "\n";
  let le: number;
  if (endsAtNewline) {
    // selEnd-1 is the "\n" of a "\r\n" pair — the "\r" belongs to it.
    le = selEnd - 1;
    if (le > ls && text[le - 1] === "\r") le -= 1;
  } else {
    const nl = text.indexOf("\n", selEnd);
    le = nl === -1 ? text.length : nl;
  }
  // Prefix each line, keeping the original separators verbatim (split with a
  // capture group yields [line, sep, line, sep, …, line]).
  const parts = text.slice(ls, le).split(/(\r?\n)/);
  const out: string[] = [];
  for (let i = 0; i < parts.length; i++) {
    out.push(i % 2 === 0 ? prefix + parts[i] : parts[i]);
  }
  const replaced = out.join("");
  return {
    text: text.slice(0, ls) + replaced + text.slice(le),
    selStart: ls,
    selEnd: ls + replaced.length,
  };
}

/**
 * Insert `block` (one or more lines, e.g. a fenced code block or a section
 * break) on its own lines at the selection, replacing it, with a blank line
 * before and after so surrounding markdown (headers, lists, paragraphs)
 * doesn't merge with the block. At the very top no gap is added before; at
 * EOF a single trailing newline is added. The caret lands after the block.
 */
export function insertBlock(
  text: string,
  selStart: number,
  selEnd: number,
  block: string,
): TextSel {
  const nl = text.includes("\r\n") ? "\r\n" : "\n";
  // Style the block's OWN line endings to match the document (first collapse
  // any CRLF/CR in the block to LF, then expand to the doc's ending) so a
  // CRLF document doesn't get LF block bodies — consistent in the editor
  // itself, before normalizeForSave ever runs (review C2).
  const blockLf = block.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  const styledBlock = nl === "\n" ? blockLf : blockLf.split("\n").join("\r\n");
  // Collapse trailing blank lines/spaces before the insertion to ONE empty
  // line ("" stays "" — no gap at the very top of the document).
  const beforeCore = text.slice(0, selStart).replace(/(?:[ \t]*(?:\r?\n))+$/, "");
  const newBefore = beforeCore === "" ? "" : beforeCore + nl + nl;
  // Skip leading blank lines after the insertion, then re-add one empty line.
  const afterCore = text.slice(selEnd).replace(/^(?:[ \t]*(?:\r?\n))+/, "");
  const newAfter = afterCore === "" ? nl : nl + nl + afterCore;
  const caret = newBefore.length + styledBlock.length;
  return {
    text: newBefore + styledBlock + newAfter,
    selStart: caret,
    selEnd: caret,
  };
}

/**
 * Preserve the file's dominant line-ending style on save: when the ORIGINAL
 * file used CRLF, the edited text (whose typed newlines are LF in a
 * textarea) is normalized back to CRLF — mirroring the Rust file tools'
 * line-ending preservation. LF files are returned verbatim.
 */
export function normalizeForSave(original: string, edited: string): string {
  if (!original.includes("\r\n")) return edited;
  return edited
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "")
    .replace(/\n/g, "\r\n");
}

/**
 * Whether the editor content differs from the last loaded/saved text,
 * ignoring a difference that is only trailing newlines (adding/removing a
 * final blank line alone doesn't mark the file dirty).
 */
export function hasUnsavedChanges(original: string, edited: string): boolean {
  const tail = (s: string) => s.replace(/(?:\r?\n)+$/, "");
  return tail(original) !== tail(edited);
}
