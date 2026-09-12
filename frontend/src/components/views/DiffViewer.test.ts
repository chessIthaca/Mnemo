// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";

import { scopedResult, shouldFetchGitDiff, diffDropdownValue, needsTransientDiffOption, classifyGitDiffError, type DiffViewMode } from "./DiffViewer";

/**
 * Unit tests for the Diff tab's pure fetch predicate (the repo's
 * component-test pattern: extract pure logic, test it directly — no DOM
 * rendering; importing the component module is a module-load only).
 */

describe("shouldFetchGitDiff", () => {
  it("fetches only in git mode with no pending approval", () => {
    expect(shouldFetchGitDiff("git", false)).toBe(true);
  });

  it("never fetches in edit mode (any pending state)", () => {
    const modes: DiffViewMode[] = ["edit"];
    for (const mode of modes) {
      expect(shouldFetchGitDiff(mode, false)).toBe(false);
      expect(shouldFetchGitDiff(mode, true)).toBe(false);
    }
  });

  it("a pending file approval owns the view — no git fetch while in flight", () => {
    // The approval's Rust preview is the authoritative diff while pending;
    // when it resolves, showPending flips false and the effect re-runs.
    expect(shouldFetchGitDiff("git", true)).toBe(false);
  });
});

describe("scopedResult", () => {
  it("shows the value only when it was produced for the current scope", () => {
    // Review B3 regression pin: a stale diff/error from the previous file
    // must never render under the new file's label while its refetch is in
    // flight — the scope change hides the old value until the new one lands.
    const stored = { scope: "src/a.rs", value: "diff-A" };
    expect(scopedResult(stored, "src/a.rs")).toBe("diff-A");
    expect(scopedResult(stored, "src/b.rs")).toBeNull();
    expect(scopedResult(null, "src/a.rs")).toBeNull();
  });
});


describe("diffDropdownValue", () => {
  it("returns the selected path even when it has no captured entry (M1 regression)", () => {
    // A file_edit chip deep-links a path that is absent from planDiffs: the
    // dropdown must still label it — the body diffs exactly this file, and
    // showing another file's name here was the label/body mismatch bug.
    expect(
      diffDropdownValue({
        allScope: false,
        selectedDiffPath: "src/x.rs",
        showPending: false,
        fallbackPath: "src/other.rs",
        mode: "git",
      }),
    ).toBe("src/x.rs");
  });

  it("allScope wins over any selection", () => {
    expect(
      diffDropdownValue({
        allScope: true,
        selectedDiffPath: "src/x.rs",
        showPending: false,
        fallbackPath: null,
        mode: "git",
      }),
    ).toBe("__all__");
  });

  it("falls back like before when nothing is selected", () => {
    expect(
      diffDropdownValue({ allScope: false, selectedDiffPath: null, showPending: true, fallbackPath: "src/a.rs", mode: "git" }),
    ).toBe("");
    expect(
      diffDropdownValue({ allScope: false, selectedDiffPath: null, showPending: false, fallbackPath: "src/a.rs", mode: "git" }),
    ).toBe("src/a.rs");
    expect(
      diffDropdownValue({ allScope: false, selectedDiffPath: null, showPending: false, fallbackPath: null, mode: "git" }),
    ).toBe("__all__");
    expect(
      diffDropdownValue({ allScope: false, selectedDiffPath: null, showPending: false, fallbackPath: null, mode: "edit" }),
    ).toBe("");
  });
});

describe("needsTransientDiffOption", () => {
  it("is true only for a selected path absent from the entries", () => {
    const entries = [{ path: "src/a.rs" }];
    expect(needsTransientDiffOption("src/x.rs", entries)).toBe(true);
    expect(needsTransientDiffOption("src/a.rs", entries)).toBe(false);
    expect(needsTransientDiffOption(null, entries)).toBe(false);
  });
});

describe("classifyGitDiffError", () => {
  it("classifies the canonical not-a-repo message", () => {
    expect(classifyGitDiffError("not a git repository")).toBe("not-a-repo");
  });

  it("classifies the canonical no-commits message", () => {
    expect(classifyGitDiffError("no commits yet")).toBe("no-commits");
  });

  it("tolerates git's raw bad-revision phrasing (unborn HEAD)", () => {
    expect(classifyGitDiffError("fatal: bad revision 'HEAD'")).toBe("no-commits");
  });

  it("tolerates git's raw unknown-revision phrasing", () => {
    expect(classifyGitDiffError("fatal: unknown revision: HEAD")).toBe("no-commits");
  });

  it("returns other for an unrelated error", () => {
    expect(classifyGitDiffError("some other git failure")).toBe("other");
  });

  it("returns other for an empty message", () => {
    expect(classifyGitDiffError("")).toBe("other");
  });
});
