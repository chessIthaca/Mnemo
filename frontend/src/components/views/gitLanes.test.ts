// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Unit tests for the Git tab's pure lane-layout engine (node env — no DOM).
//
// Pins the contract the GitView SVG relies on: main's first-parent spine is
// pinned to lane 0, branches flow on side lanes, merged lanes are freed for
// reuse, spine pinning wins over lane-0 reuse, unknown parents (beyond the
// backend's commit cap) never extend a lane, and the color mapping cycles.

import { describe, expect, it } from "vitest";

import type { GitCommitInfo } from "../../lib/tauri";
import {
  computeBranchBases,
  computeLanes,
  countGitMutations,
  countGitMutationsCached,
  isGitMutation,
  laneColor,
  shortDate,
  visibleCommits,
} from "./gitLanes";

/** Minimal GitCommitInfo fixture (defaults suffice for lane math). */
function c(sha: string, parents: string[], subject = `subj ${sha}`): GitCommitInfo {
  return {
    sha,
    short_sha: sha.slice(0, 7),
    parents,
    subject,
    author: "Test",
    timestamp: 1_787_000_000,
    refs: [],
  };
}

/** lane-by-sha lookup from a layout. */
function laneOf(layout: ReturnType<typeof computeLanes>, sha: string): number {
  const lc = layout.commits.find((x) => x.sha === sha);
  if (!lc) throw new Error(`sha ${sha} not in layout`);
  return lc.lane;
}

describe("computeLanes", () => {
  it("pins a linear main history entirely to lane 0", () => {
    // I → S → M, all spine: one lane, everything on it.
    const commits = [c("M", ["S"]), c("S", ["I"]), c("I", [])];
    const layout = computeLanes(commits, new Set(["M", "S", "I"]));
    expect(layout.laneCount).toBe(1);
    expect(layout.commits.map((x) => x.lane)).toEqual([0, 0, 0]);
  });

  it("puts branch commits on side lanes and frees them after a merge", () => {
    // main: I → S → M (merge of F) → M2 (merge of H)
    // feat/x: S → F;  feat2: M → H
    // Topo order (newest first): M2, H, M, F, S, I.
    const commits = [
      c("M2", ["M", "H"], "merge feat2"),
      c("H", ["M"], "feature 2"),
      c("M", ["S", "F"], "merge feat/x"),
      c("F", ["S"], "feature 1"),
      c("S", ["I"], "second on main"),
      c("I", [], "initial"),
    ];
    const spine = new Set(["M2", "M", "S", "I"]);
    const layout = computeLanes(commits, spine);

    // Spine on lane 0; both branch commits on lane 1 (H REUSES the lane F
    // freed when M consumed its expectation — the merge freed it).
    expect(laneOf(layout, "M2")).toBe(0);
    expect(laneOf(layout, "M")).toBe(0);
    expect(laneOf(layout, "S")).toBe(0);
    expect(laneOf(layout, "I")).toBe(0);
    expect(laneOf(layout, "F")).toBe(1);
    expect(laneOf(layout, "H")).toBe(1);
    expect(layout.laneCount).toBe(2);
  });

  it("keeps spine pinning even when a spine commit's first parent is off-spine", () => {
    // A (spine, e.g. a squashed merge whose first parent B never made the
    // first-parent chain) — B must NOT inherit lane 0; it gets a side lane.
    const commits = [c("A", ["B"]), c("B", ["I"]), c("I", [])];
    const layout = computeLanes(commits, new Set(["A", "I"]));
    expect(laneOf(layout, "A")).toBe(0);
    expect(laneOf(layout, "B")).toBe(1);
    expect(laneOf(layout, "I")).toBe(0);
    expect(layout.laneCount).toBe(2);
  });

  it("returns an empty single-lane layout for no commits", () => {
    const layout = computeLanes([], new Set(["main-only"]));
    expect(layout.commits).toEqual([]);
    expect(layout.laneCount).toBe(1);
  });

  it("tolerates unknown parents without extending or crashing", () => {
    // Q is the spine tip; P is a SIDE-lANE commit (not spine) whose first
    // parent is beyond the backend's -n cap: P still gets a lane, the
    // parent is preserved verbatim for the connector, and no lane is left
    // eternally expecting a sha that never arrives.
    const commits = [c("Q", ["P"]), c("P", ["ghost"])]; // Q newest
    const layout = computeLanes(commits, new Set(["Q"]));
    expect(laneOf(layout, "P")).toBeGreaterThanOrEqual(1); // side lane
    expect(laneOf(layout, "Q")).toBe(0);
    const p = layout.commits.find((x) => x.sha === "P")!;
    expect(p.parents).toEqual(["ghost"]); // preserved for rendering
    expect(layout.laneCount).toBeGreaterThanOrEqual(2);
  });

  it("starts side lanes at 0 when there is no main spine", () => {
    // A repo without a `main` branch: nothing is spine, so lane 0 is not
    // reserved — the first branch uses it instead of wasting it.
    const commits = [c("X", ["Y"]), c("Y", [])];
    const layout = computeLanes(commits, new Set());
    expect(layout.commits.map((x) => x.lane)).toEqual([0, 0]);
    expect(layout.laneCount).toBe(1);
  });

  it("keeps lane 0 reserved for the spine when the FIRST commit is off-spine (branch ahead of main)", () => {
    // Regression (review B1): freeLane(1) on an empty lanes array used to
    // return lane 0, putting a branch-ahead-of-main's tip ON the spine
    // lane. Topo order puts the branch's exclusive commits FIRST when the
    // branch is newer than main's tip — the common open-branch state.
    // main: I → S; feat: S → F1 → F2 (feat ahead). Topo: F2, F1, S, I.
    const commits = [
      c("F2", ["F1"], "branch tip"),
      c("F1", ["S"], "branch work"),
      c("S", ["I"], "main tip"),
      c("I", [], "initial"),
    ];
    const layout = computeLanes(commits, new Set(["S", "I"]));
    expect(laneOf(layout, "F2")).toBe(1); // NOT lane 0 — spine reserved
    expect(laneOf(layout, "F1")).toBe(1);
    expect(laneOf(layout, "S")).toBe(0);
    expect(laneOf(layout, "I")).toBe(0);
    expect(layout.laneCount).toBe(2);
  });
});

describe("laneColor", () => {
  it("renders the spine in the theme accent", () => {
    expect(laneColor(0, true)).toBe("var(--accent-color)");
    expect(laneColor(3, true)).toBe("var(--accent-color)");
  });

  it("cycles side lanes through the palette with wraparound", () => {
    expect(laneColor(1, false)).toBe("#a78bfa"); // violet
    expect(laneColor(2, false)).toBe("#34d399"); // emerald
    expect(laneColor(6, false)).toBe("#a78bfa"); // (6-1) mod 5 → wraps to violet
  });
});

describe("shortDate", () => {
  it("returns a compact non-empty label", () => {
    const label = shortDate(1_787_000_000);
    expect(typeof label).toBe("string");
    expect(label.length).toBeGreaterThan(0);
  });
});

// ── computeBranchBases ─────────────────────────────────────────────────────

/** Shared DAG fixture: main: I → S → M (merge of F) → M2 (merge of H);
 *  feat/x: S → F; feat2: M → H. Topo order newest first. */
function dagFixture() {
  const commits = [
    c("M2", ["M", "H"], "merge feat2"),
    c("H", ["M"], "feature 2"),
    c("M", ["S", "F"], "merge feat/x"),
    c("F", ["S"], "feature 1"),
    c("S", ["I"], "second on main"),
    c("I", [], "initial"),
  ];
  const spine = new Set(["M2", "M", "S", "I"]);
  const branches = [
    { name: "main", is_main: true, tip_sha: "M2" },
    { name: "feat/x", is_main: false, tip_sha: "F" },
    { name: "feat2", is_main: false, tip_sha: "H" },
  ];
  return { commits, spine, branches };
}

describe("computeBranchBases", () => {
  it("resolves each branch's fork point to the nearest main ancestor", () => {
    const { commits, spine, branches } = dagFixture();
    const bases = computeBranchBases(commits, spine, branches);
    // feat/x cut from S: F's nearest spine ancestor is S.
    expect(bases.get("feat/x")).toBe("S");
    // feat2 cut from M: H's parent IS M (spine) → base M.
    expect(bases.get("feat2")).toBe("M");
    // main itself never gets an entry.
    expect(bases.has("main")).toBe(false);
  });

  it("resolves a branch cut from another branch to the nearest MAIN ancestor", () => {
    const { commits, spine } = dagFixture();
    // G branches off feat/x (parent F, itself off-spine): the base is
    // feat/x's own base (S), not F — "based on" means the main fork point.
    const withG = [...commits, c("G", ["F"], "off feat/x")];
    const branches = [
      { name: "feat/x", is_main: false, tip_sha: "F" },
      { name: "feat/y", is_main: false, tip_sha: "G" },
    ];
    const bases = computeBranchBases(withG, spine, branches);
    expect(bases.get("feat/y")).toBe("S");
  });

  it("uses the tip itself as the base when the tip sits on main (fresh fork — review finding 1)", () => {
    // A freshly forked branch (create_plan's auto-fork, before its first
    // commit) points AT a main commit: the fork point IS the tip, not the
    // tip's ancestor. Same for a stale branch pinned to an older main commit.
    const commits = [c("M", ["S"]), c("S", ["I"]), c("I", [])];
    const spine = new Set(["M", "S", "I"]);
    const bases = computeBranchBases(commits, spine, [
      { name: "feat/fresh", is_main: false, tip_sha: "S" },
    ]);
    expect(bases.get("feat/fresh")).toBe("S");
  });

  it("omits branches forked beyond the loaded window instead of guessing", () => {
    // Tip P's parent is a ghost sha (beyond the backend's commit cap): no
    // spine ancestor is reachable → no entry.
    const commits = [c("P", ["ghost"]), c("Q", [])];
    const spine = new Set(["Q"]);
    const bases = computeBranchBases(commits, spine, [
      { name: "feat/old", is_main: false, tip_sha: "P" },
    ]);
    expect(bases.has("feat/old")).toBe(false);
  });

  it("returns an empty map when there is no main spine", () => {
    const commits = [c("X", ["Y"]), c("Y", [])];
    const bases = computeBranchBases(commits, new Set(), [
      { name: "x", is_main: false, tip_sha: "X" },
    ]);
    expect(bases.size).toBe(0);
  });
});

// ── visibleCommits ─────────────────────────────────────────────────────────

/** 25-commit spine (s25 newest … s1 oldest, input topo order newest first)
 *  plus two side commits off s10 and s24. */
function longMainFixture() {
  const spineShas = Array.from({ length: 25 }, (_, k) => `s${25 - k}`); // newest first
  const commits: GitCommitInfo[] = spineShas.map((sha, i) =>
    c(sha, i < spineShas.length - 1 ? [spineShas[i + 1]] : []),
  );
  // Insert side commits at their topo positions (after their base).
  commits.splice(10, 0, c("sideNew", ["s10"])); // newer than s10
  commits.splice(25, 0, c("sideOld", ["s24"]));
  const spine = new Set(spineShas);
  return { commits, spine, spineShas };
}

describe("visibleCommits", () => {
  it("collapses main to the newest 20 spine commits, keeping every side commit", () => {
    const { commits, spine, spineShas } = longMainFixture();
    const v = visibleCommits(commits, spine, false);
    // All 27 input commits − 5 hidden old spine rows.
    expect(v.visible.length).toBe(27 - 5);
    expect(v.hiddenSpineCount).toBe(5);
    // Both side commits survive regardless of their age.
    expect(v.visible.some((x) => x.sha === "sideNew")).toBe(true);
    expect(v.visible.some((x) => x.sha === "sideOld")).toBe(true);
    // The newest 20 spine commits survive; the 5 oldest don't.
    spineShas.slice(0, 20).forEach((sha) => {
      expect(v.visible.some((x) => x.sha === sha)).toBe(true);
    });
    spineShas.slice(20).forEach((sha) => {
      expect(v.visible.some((x) => x.sha === sha)).toBe(false);
    });
  });

  it("preserves input (topo) order in the visible slice", () => {
    const { commits, spine } = longMainFixture();
    for (const expanded of [false, true]) {
      const v = visibleCommits(commits, spine, expanded);
      const idx = new Map(v.visible.map((x, i) => [x.sha, i]));
      commits.forEach((orig) => {
        const at = idx.get(orig.sha);
        if (at !== undefined) expect(at).toBeLessThan(v.visible.length);
      });
      // The visible list must be a subsequence of the input.
      let cursor = 0;
      for (const orig of commits) {
        if (idx.has(orig.sha)) {
          expect(idx.get(orig.sha)).toBeGreaterThanOrEqual(cursor);
          cursor = idx.get(orig.sha)!;
        }
      }
    }
  });

  it("returns everything when expanded", () => {
    const { commits, spine } = longMainFixture();
    const v = visibleCommits(commits, spine, true);
    expect(v.visible.length).toBe(27);
    expect(v.hiddenSpineCount).toBe(0);
  });

  it("keeps a short main history fully visible without expanding", () => {
    const commits = [c("M", ["S"]), c("S", ["I"]), c("I", []), c("F", ["S"])];
    const spine = new Set(["M", "S", "I"]);
    const v = visibleCommits(commits, spine, false);
    expect(v.visible.length).toBe(4);
    expect(v.hiddenSpineCount).toBe(0);
  });

  it("counts only LOADED spine commits toward the collapse (review finding 2)", () => {
    // The backend caps main_spine and commits at 400 INDEPENDENTLY, so the
    // spine set can name commits the log window never loaded. Those are
    // neither renderable nor expandable and must not inflate the hidden
    // count (the expand row must not overpromise).
    const spineShas = Array.from({ length: 25 }, (_, k) => `s${25 - k}`);
    const loaded = spineShas.slice(0, 22); // 22 of the 25 spine shas loaded
    const commits: GitCommitInfo[] = loaded.map((sha, i) =>
      c(sha, i < loaded.length - 1 ? [loaded[i + 1]] : []),
    );
    commits.push(c("side", ["s10"]));
    const v = visibleCommits(commits, new Set(spineShas), false);
    expect(v.hiddenSpineCount).toBe(2); // 22 loaded − 20 shown (NOT 25 − 20)
    expect(v.visible.length).toBe(21); // 20 newest spine + the side commit
  });
});

// ── countGitMutations / isGitMutation ──────────────────────────────────────

/** One completed tool call as a transcript entry. */
function toolEntry(name: string, args: unknown, success = true) {
  return {
    kind: "tool" as const,
    name,
    calls: [{ args: JSON.stringify(args), result: { success } }],
  };
}

describe("countGitMutations", () => {
  it("counts mutating git/shell/create_plan calls, ignoring reads + in-flight + failed", () => {
    const transcript = [
      toolEntry("git", { subcommand: "status" }), // read-only
      toolEntry("git", { subcommand: "commit", message: "m" }), // ✓
      toolEntry("git", { subcommand: "checkout", branch: "main" }), // ✓
      toolEntry("git", { subcommand: "log" }), // read-only
      toolEntry("git", { subcommand: "branch", action: "create", branch: "fix/x" }), // ✓
      toolEntry("git", { subcommand: "branch", action: "list" }), // read-only list
      toolEntry("git", { subcommand: "branch" }), // list default — read-only
      toolEntry("shell", { command: "cargo test" }), // not git
      toolEntry("shell", { command: "git commit -m x" }), // ✓
      toolEntry("create_plan", { title: "T", steps: ["a"], branch: "fix/x" }), // ✓
      toolEntry("create_plan", { title: "T", steps: ["a"] }), // no branch
      { kind: "tool", name: "git", calls: [{ args: '{"subcommand":"merge"}', result: null }] }, // in-flight
      toolEntry("git", { subcommand: "branch", action: "delete" }, false), // failed
      { kind: "assistant", name: "" }, // non-tool entry — ignored
    ];
    expect(countGitMutations(transcript)).toBe(5);
  });

  it("returns 0 for an empty transcript", () => {
    expect(countGitMutations([])).toBe(0);
  });

  it("countGitMutationsCached memoizes by transcript identity (review finding 4)", () => {
    const t1 = [toolEntry("git", { subcommand: "commit", message: "m" })];
    expect(countGitMutationsCached(t1)).toBe(1);
    expect(countGitMutationsCached(t1)).toBe(1); // same array — cache hit
    // A different array (a new event appended) must be recomputed, proving
    // the cache keys on identity, not a single remembered value.
    const t2 = [...t1, toolEntry("git", { subcommand: "commit", message: "m2" })];
    expect(countGitMutationsCached(t2)).toBe(2);
  });
});

describe("isGitMutation", () => {
  it("matches mutating git subcommands and rejects read-only ones", () => {
    for (const sub of ["commit", "merge", "checkout"]) {
      expect(isGitMutation("git", `{"subcommand":"${sub}"}`)).toBe(true);
    }
    for (const sub of ["status", "diff", "log", "stash"]) {
      expect(isGitMutation("git", `{"subcommand":"${sub}"}`)).toBe(false);
    }
  });

  it("treats git branch as mutating ONLY on create/delete (list default is read-only — review finding 3)", () => {
    expect(
      isGitMutation("git", JSON.stringify({ subcommand: "branch", action: "create" })),
    ).toBe(true);
    expect(
      isGitMutation("git", JSON.stringify({ subcommand: "branch", action: "delete" })),
    ).toBe(true);
    expect(
      isGitMutation("git", JSON.stringify({ subcommand: "branch", action: "list" })),
    ).toBe(false);
    expect(isGitMutation("git", JSON.stringify({ subcommand: "branch" }))).toBe(false);
  });

  it("falls back to substring matching on malformed git args", () => {
    expect(isGitMutation("git", `{"subcommand":"commit" broken json`)).toBe(true);
    expect(isGitMutation("git", `{"subcommand":"status" broken json`)).toBe(false);
    // Malformed branch args count only with a literally-present mutating action.
    expect(isGitMutation("git", `{"subcommand":"branch","action":"create" oops`)).toBe(true);
    expect(isGitMutation("git", `{"subcommand":"branch","action":"list" oops`)).toBe(false);
    expect(isGitMutation("git", `{"subcommand":"branch" oops`)).toBe(false);
  });

  it("matches shell git invocations incl. leading flags, rejecting non-git", () => {
    expect(isGitMutation("shell", JSON.stringify({ command: "git commit -m x" }))).toBe(true);
    expect(isGitMutation("shell", JSON.stringify({ command: "git -c a=b merge feat" }))).toBe(true);
    expect(isGitMutation("shell", JSON.stringify({ command: "git checkout main" }))).toBe(true);
    expect(isGitMutation("shell", JSON.stringify({ command: "git status" }))).toBe(false);
    expect(isGitMutation("shell", JSON.stringify({ command: "cargo test" }))).toBe(false);
    expect(isGitMutation("shell", JSON.stringify({ command: "echo git commit" }))).toBe(false);
  });

  it("matches create_plan only with a non-empty branch arg", () => {
    expect(isGitMutation("create_plan", JSON.stringify({ branch: "fix/x" }))).toBe(true);
    expect(isGitMutation("create_plan", JSON.stringify({ branch: "" }))).toBe(false);
    expect(isGitMutation("create_plan", JSON.stringify({ title: "T" }))).toBe(false);
    expect(isGitMutation("create_plan", "not json")).toBe(false);
  });

  it("rejects other tools outright", () => {
    expect(isGitMutation("file_edit", '{"path":"a"}')).toBe(false);
  });
});
