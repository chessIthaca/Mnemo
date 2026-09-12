// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * SourceEditor contract tests.
 *
 * SourceEditor is THE shared file editor (extracted from FileViewer): the
 * Files tab's bottom pane and the Graph tab's browse-to-source pane must
 * render the same component — a second copy of the toolbar/edit logic
 * (the pre-extraction state) would silently drift.
 *
 * This project has no React DOM test infra (vitest runs in `node`
 * environment), so the structural assertions are static source-contract
 * tests in the style of `src/lib/ipc-contract.test.ts` (Vite `?raw` import,
 * typed by `vite/client`); `clampRevealScroll` is a pure function and is
 * tested directly.
 */

import { describe, expect, it } from "vitest";
import { clampRevealScroll, shouldPersistStraddledRead } from "./SourceEditor";
import source from "./SourceEditor.tsx?raw";
import fileViewerSource from "../views/FileViewer.tsx?raw";

describe("clampRevealScroll", () => {
  it("line 1 and below scroll nowhere (the file starts at the top)", () => {
    expect(clampRevealScroll(1, 100, 20)).toBe(0);
    expect(clampRevealScroll(0, 100, 20)).toBe(0);
    expect(clampRevealScroll(-5, 100, 20)).toBe(0);
  });

  it("a mid-file line lands on (line - 1) * perLinePx", () => {
    expect(clampRevealScroll(10, 100, 20)).toBe(9 * 20);
    expect(clampRevealScroll(42, 500, 13.5)).toBe(41 * 13.5);
  });

  it("a line beyond the end clamps to the last line", () => {
    expect(clampRevealScroll(150, 100, 20)).toBe(99 * 20);
    expect(clampRevealScroll(100, 100, 20)).toBe(99 * 20);
  });

  it("degenerate inputs never produce a negative or NaN offset", () => {
    expect(clampRevealScroll(10, 0, 20)).toBe(0);
    expect(clampRevealScroll(10, 100, 0)).toBe(0);
    expect(clampRevealScroll(10, 100, -1)).toBe(0);
  });
});

describe("shouldPersistStraddledRead (review H1 regression)", () => {
  // A read that resolves AFTER the component unmounted must still write the
  // session — otherwise the remount restores the PREVIOUS file's body under
  // the new path and Save can write it across files. True ONLY when the read
  // was for the path the session still holds (a superseded read must not
  // clobber the newer file's session).
  it("persists only a disposed read for the session's current path", () => {
    expect(shouldPersistStraddledRead("src/a.rs", "src/a.rs", true, true)).toBe(true);
  });

  it("skips a superseded read (session path moved on)", () => {
    expect(shouldPersistStraddledRead("src/b.rs", "src/a.rs", true, true)).toBe(false);
  });

  it("skips mounted reads (the mirror effect owns those) and non-persisting instances", () => {
    expect(shouldPersistStraddledRead("src/a.rs", "src/a.rs", false, true)).toBe(false);
    expect(shouldPersistStraddledRead("src/a.rs", "src/a.rs", true, false)).toBe(false);
  });
});

describe("SourceEditor extraction contract", () => {
  it("FileViewer renders the shared SourceEditor (no duplicated editor)", () => {
    expect(fileViewerSource).toContain("<SourceEditor");
    // The markdown toolbar moved wholesale — a copy left behind in
    // FileViewer would be the drift signal.
    expect(fileViewerSource).not.toContain("TOOLBAR_BUTTONS");
    expect(fileViewerSource).not.toContain("wrapCodeFence");
  });

  it("FileViewer opts into the persisted editor session", () => {
    // The B1 contract: a dirty edit survives the Files tab unmounting.
    expect(fileViewerSource).toContain("persistSession");
  });

  it("keeps the code-fence rendering path (syntax highlighting)", () => {
    expect(source).toContain("wrapCodeFence");
  });

  it("keeps line-ending-preserving saves and the Ctrl+S binding", () => {
    expect(source).toContain("normalizeForSave");
    expect(source).toContain('title="Save (Ctrl+S)"');
    expect(source).toContain('(e.ctrlKey || e.metaKey) && (e.key === "s" || e.key === "S")');
  });

  it("session persistence stays opt-in (two live instances never fight)", () => {
    // The Graph tab's peek view must NOT mirror into the Files tab's
    // session — only persistSession consumers touch editorSession.
    expect(source).toContain("if (!persistSession) return;");
  });

  it("every explicit open re-loads via reloadToken (review M1 regression)", () => {
    // Static-source ceiling note: there is no DOM test infra here, so the
    // practical pin is that the token sits in the load effect's dep array
    // AND FileViewer bumps it on every open path. Without the dep, a
    // confirmed "discard?" on the already-open file silently kept the edits.
    expect(source).toContain("}, [path, browsed, persistSession, reloadToken]);");
    expect(fileViewerSource).toContain("setReloadToken((t) => t + 1)");
    expect(fileViewerSource).toContain("reloadToken={reloadToken}");
  });

  it("the reveal effect re-runs on reloadToken (review LOW 1: same-file/same-line re-click re-scrolls)", () => {
    // The reveal effect's deps were [content, revealLine, path] — a re-click
    // on the SAME file at the SAME line bumped reloadToken and re-read
    // byte-identical content, so every dep stayed Object.is-equal and the
    // effect never re-ran (no re-scroll). The token must sit in the dep
    // array; manual opens reset revealLine to null, so the guard keeps them
    // no-ops, and GraphView never changes the token (default 0).
    expect(source).toContain("}, [content, revealLine, path, reloadToken]);");
  });

  it("a restored session keeps its edit mode (review L2 regression)", () => {
    // The restoring early-return must run BEFORE the setEditing(false)
    // reset — otherwise a remount mid-edit kicks the user out of edit mode.
    expect(source).toMatch(/if \(restoring\) return;[\s\S]*?setEditing\(false\);/);
  });

  it("straddle writes go through the path-checked helper in both arms (review H1)", () => {
    const uses = source.match(
      /shouldPersistStraddledRead\(editorSession\.path, path, true, persistSession\)/g,
    ) ?? [];
    expect(uses.length).toBe(2); // the resolved arm AND the failure arm
  });

  it("the reveal effect has no dead textarea branch (review N1)", () => {
    // The edit-mode textarea only renders for markdown, which the effect
    // returns early on — a taRef branch there was unreachable dead code.
    expect(source).not.toContain("ta.scrollTop");
  });
});
