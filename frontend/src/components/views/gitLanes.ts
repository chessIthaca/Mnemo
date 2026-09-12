// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Pure lane-layout engine for the Git tab's commit DAG (Maint: no React/DOM
// imports — this module runs in the node-env vitest suite).
//
// Given the commit list (topo order, newest first) and main's first-parent
// spine, assigns each commit a horizontal "lane" so the DAG renders as the
// familiar git graph: main's spine is PINNED to lane 0 (rendered
// predominantly in the accent color), feature branches flow on side lanes,
// and lanes are freed after a merge so a later branch reuses them.
//
// The algorithm is the classic active-lane sweep: `lanes[i]` holds the sha
// expected to appear next on lane i (null = free). Placing a commit
// consumes its expectation, clears any duplicate expectations (branch
// convergence at a merge), then extends lanes to its parents — first parent
// keeps the commit's lane, additional parents claim free lanes; spine
// parents always land on lane 0, and unknown parents (beyond the backend's
// commit cap) extend nothing.
//
// Also hosts the view's other pure helpers: `computeBranchBases` (each
// branch's fork point on main), `visibleCommits` (collapse main's spine to
// the newest few rows with an expand path), and `countGitMutations` (the
// live-refresh trigger derived from agent transcripts).

import type { GitCommitInfo } from "../../lib/tauri";

/** One commit with its assigned lane, in the same order as the input. */
export interface LaneCommit {
  /** Full commit sha. */
  sha: string;
  /** Assigned lane (0 = main's spine when a spine exists). */
  lane: number;
  /** Parent shas verbatim (including unknown ones, for connectors). */
  parents: string[];
}

/** The lane assignment for a whole commit list. */
export interface LaneLayout {
  /** Each input commit, in input order, with its lane. */
  commits: LaneCommit[];
  /** Number of lanes to render (max lane + 1, min 1). */
  laneCount: number;
}

/** Side-lane color cycle (violet, emerald, amber, pink, sky). */
const BRANCH_PALETTE = ["#a78bfa", "#34d399", "#fbbf24", "#f472b6", "#38bdf8"] as const;

/**
 * The stroke/dot color for a lane: the spine renders in the theme accent
 * (`var(--accent-color)`), side lanes cycle the branch palette (negative /
 * zero lanes are guarded for the no-`main` case where lane 0 is not the
 * spine).
 */
export function laneColor(lane: number, isSpine: boolean): string {
  if (isSpine) return "var(--accent-color)";
  const n = BRANCH_PALETTE.length;
  const idx = ((lane - 1) % n + n) % n;
  return BRANCH_PALETTE[idx];
}

/**
 * Assign lanes: main's first-parent spine is pinned to lane 0 (which is
 * reserved for it whenever the spine is non-empty); non-spine commits take
 * the lane expecting them or the first free side lane; merges free the
 * merged branch's lane. Unknown parents never extend a lane but are kept
 * in the output so the renderer can draw clipped connectors.
 */
export function computeLanes(
  commits: readonly GitCommitInfo[],
  mainSpine: ReadonlySet<string>,
): LaneLayout {
  const known = new Set(commits.map((c) => c.sha));
  const spineExists = mainSpine.size > 0;
  // lanes[i] = the sha expected next on lane i; null = free. Lane 0 is
  // reserved for the spine when one exists.
  const lanes: Array<string | null> = [];

  /** The first free lane at/after `from` (growing the array as needed —
   *  never returns a lane below `from`, so callers can reserve lane 0 for
   *  the spine by passing 1). */
  const freeLane = (from: number): number => {
    while (lanes.length <= from) lanes.push(null);
    return from;
  };

  /** Claim `sha` on a side lane: reuse a lane already expecting it, else
   * the first free side lane. No-op for unknown shas. */
  const sideClaim = (sha: string): void => {
    if (!known.has(sha)) return;
    const existing = lanes.indexOf(sha);
    if (existing !== -1) {
      // Reuse — but never steal lane 0 from the spine.
      if (!(spineExists && existing === 0)) return;
    }
    lanes[freeLane(spineExists ? 1 : 0)] = sha;
  };

  /** Extend a spine parent's expectation onto lane 0 (growing as needed). */
  const spineClaim = (sha: string): void => {
    while (lanes.length < 1) lanes.push(null);
    lanes[0] = sha;
  };

  const out: LaneCommit[] = [];
  for (const c of commits) {
    const isSpine = mainSpine.has(c.sha);

    // 1. Pick the lane: spine → 0 (reserved); else the lane expecting this
    //    sha, else the first free side lane.
    let lane: number;
    if (isSpine) {
      lane = 0;
      while (lanes.length < 1) lanes.push(null);
    } else {
      const expecting = lanes.indexOf(c.sha);
      lane = expecting !== -1 && !(spineExists && expecting === 0)
        ? expecting
        : freeLane(spineExists ? 1 : 0);
    }

    // 2. Consume this commit's expectation + clear duplicates (converged
    //    branches that also pointed here).
    lanes[lane] = null;
    for (let i = 0; i < lanes.length; i++) {
      if (i !== lane && lanes[i] === c.sha) lanes[i] = null;
    }

    // 3. Extend to parents: spine parents → lane 0; the first parent of a
    //    NON-spine commit keeps its lane; other parents claim side lanes.
    c.parents.forEach((p, idx) => {
      if (mainSpine.has(p)) {
        spineClaim(p);
      } else if (idx === 0 && !isSpine && known.has(p)) {
        lanes[lane] = p;
      } else {
        sideClaim(p);
      }
    });

    out.push({ sha: c.sha, lane, parents: [...c.parents] });
  }

  const laneCount = out.reduce((m, lc) => Math.max(m, lc.lane + 1), 1);
  return { commits: out, laneCount };
}

/** How many of main's newest spine commits render when collapsed. */
export const SPINE_COLLAPSE_LIMIT = 20;

/**
 * Each branch's BASE (fork point) — the nearest ancestor of the branch tip
 * that sits on main's first-parent spine, INCLUDING the tip itself when the
 * tip is a spine commit (a freshly forked branch before its first commit —
 * exactly what `create_plan`'s auto-fork produces — points AT main, so its
 * fork point IS its tip; review finding 1). BFS from the tip over the loaded
 * parents finds the shortest path, so the first spine commit reached IS the
 * nearest one. Branches cut from another branch resolve to that branch's own
 * base (the nearest MAIN ancestor), which is what "based on" means here.
 *
 * Returns branch name → base sha. Branches with no spine ancestor inside the
 * loaded window (forked beyond the backend's commit cap) get NO entry — the
 * view hides the base indicator rather than guessing.
 */
export function computeBranchBases(
  commits: readonly GitCommitInfo[],
  mainSpine: ReadonlySet<string>,
  branches: readonly { name: string; is_main: boolean; tip_sha: string }[],
): Map<string, string> {
  const parentsOf = new Map(commits.map((c) => [c.sha, c.parents] as const));
  const bases = new Map<string, string>();
  for (const b of branches) {
    if (b.is_main) continue;
    if (!parentsOf.has(b.tip_sha)) continue;
    // BFS from the tip; the first sha reached that is on the spine is the
    // base — the tip itself when the branch was just cut from main.
    const queue: string[] = [b.tip_sha];
    const seen = new Set(queue);
    while (queue.length > 0) {
      const sha = queue.shift()!;
      if (mainSpine.has(sha)) {
        bases.set(b.name, sha);
        break;
      }
      for (const p of parentsOf.get(sha) ?? []) {
        if (!seen.has(p) && parentsOf.has(p)) {
          seen.add(p);
          queue.push(p);
        }
      }
    }
  }
  return bases;
}

/** The render-time slice of the commit list (see [`visibleCommits`]). */
export interface VisibleCommits {
  /** Commits to render, in input (topo) order. */
  visible: GitCommitInfo[];
  /** Spine commits hidden by the collapse (0 when fully expanded). */
  hiddenSpineCount: number;
}

/**
 * Collapse main's spine to its newest [`SPINE_COLLAPSE_LIMIT`] commits while
 * keeping every side-branch commit visible — a long main history stops
 * dominating the graph until the user expands. `expanded` returns everything.
 *
 * The input (topo) order is preserved: filtering only DROPS old spine rows,
 * so the lane layout (computed over the FULL set) and connector geometry stay
 * consistent. Side branches whose base is hidden keep rendering with a
 * clipped connector stub — the expand control below the rows is the
 * affordance that reveals it.
 */
export function visibleCommits(
  commits: readonly GitCommitInfo[],
  mainSpine: ReadonlySet<string>,
  expanded: boolean,
  limit: number = SPINE_COLLAPSE_LIMIT,
): VisibleCommits {
  // Rank spine commits by their position in the INPUT list (topo, newest
  // first) — counting only shas actually present in `commits`. The backend
  // caps `main_spine` and `commits` at 400 INDEPENDENTLY, so in a big repo
  // the spine set can name commits the log window never loaded; those are
  // neither renderable nor expandable and must not inflate
  // `hiddenSpineCount` (review finding 2). Deriving the rank from the array
  // (instead of Set iteration order) also makes the newest-first premise
  // explicit in this module's own contract.
  const spineRank = new Map<string, number>();
  for (const c of commits) {
    if (mainSpine.has(c.sha)) spineRank.set(c.sha, spineRank.size);
  }
  const totalSpine = spineRank.size;
  const keepAll = expanded || totalSpine <= limit;
  const visible = keepAll
    ? [...commits]
    : commits.filter(
        (c) => !spineRank.has(c.sha) || (spineRank.get(c.sha) ?? 0) < limit,
      );
  const keptSpine = keepAll ? totalSpine : Math.min(totalSpine, limit);
  return { visible, hiddenSpineCount: Math.max(0, totalSpine - keptSpine) };
}

/**
 * Count completed, SUCCESSFUL tool calls in one agent's transcript that
 * mutate the repository's branch/commit graph — the Git tab's live-refresh
 * trigger ("update whenever an agent commits, branches, or merges"). Covers
 * three surfaces: the `git` tool with a mutating subcommand
 * (commit/merge/checkout/branch), a `shell` command invoking those git
 * verbs, and `create_plan` with a `branch` param (the automatic feature
 * fork). Read-only operations (status/diff/log), in-flight calls (result
 * still null), and failures don't count — they don't change the graph.
 *
 * Structural typing (no store imports): matches the transcript tool-entry
 * shape produced by the agent-event reducers.
 */
export function countGitMutations(
  transcript: ReadonlyArray<{
    kind: string;
    name?: string;
    calls?: ReadonlyArray<{
      args: string;
      result: { success: boolean } | null;
    }>;
  }>,
): number {
  let count = 0;
  for (const entry of transcript) {
    if (entry.kind !== "tool" || !entry.calls) continue;
    for (const call of entry.calls) {
      if (call.result === null || !call.result.success) continue;
      if (isGitMutation(entry.name ?? "", call.args)) count += 1;
    }
  }
  return count;
}

/** Memo table for [`countGitMutationsCached`], keyed by transcript identity.
 *  Weak so a replaced/dropped agent's array is collected with the key. */
const mutationCountCache = new WeakMap<object, number>();

/**
 * [`countGitMutations`] memoized by the transcript array's IDENTITY.
 * Transcripts are immutably replaced per agent (every transcript event builds
 * a fresh array in the reducers), so an unchanged agent is an O(1) cache hit —
 * the Git tab's store selector no longer re-walks and re-parses every
 * historical git/shell/create_plan call's args on each store notify (review
 * finding 4). Cache misses fall through to the pure counter.
 */
export function countGitMutationsCached(
  transcript: ReadonlyArray<{
    kind: string;
    name?: string;
    calls?: ReadonlyArray<{
      args: string;
      result: { success: boolean } | null;
    }>;
  }>,
): number {
  const hit = mutationCountCache.get(transcript);
  if (hit !== undefined) return hit;
  const n = countGitMutations(transcript);
  mutationCountCache.set(transcript, n);
  return n;
}

/** The git subcommands that change the branch/commit graph (not reads).
 *  `branch` is handled specially in [`isGitMutation`]: its LIST action (the
 *  default) is read-only, so only create/delete count. */
const GIT_MUTATING_SUBCOMMANDS = new Set(["commit", "merge", "checkout", "branch"]);

/**
 * Whether one completed tool call mutated the repo graph. Pure; exported for
 * tests. Malformed args JSON degrades to substring matching (never throws).
 */
export function isGitMutation(toolName: string, argsJson: string): boolean {
  if (toolName === "git") {
    let sub: string | undefined;
    let action: string | undefined;
    try {
      const parsed = JSON.parse(argsJson) as { subcommand?: string; action?: string };
      sub = parsed.subcommand;
      action = parsed.action;
    } catch {
      sub = undefined;
    }
    if (sub !== undefined) {
      if (sub === "branch") {
        // `branch` defaults to LIST (read-only) when the action is omitted —
        // only create/delete mutate the graph (review finding 3; mirrors the
        // backend's is_git_read_only classification).
        return action === "create" || action === "delete";
      }
      return GIT_MUTATING_SUBCOMMANDS.has(sub);
    }
    // Fallback for unparseable args: look for the literal field text. A
    // `branch` match counts only when a mutating action is literally present.
    return [...GIT_MUTATING_SUBCOMMANDS].some((s) => {
      if (s === "branch") return /"action"\s*:\s*"(?:create|delete)"/.test(argsJson);
      return (
        argsJson.includes(`"subcommand":"${s}"`) ||
        argsJson.includes(`"subcommand": "${s}"`)
      );
    });
  }
  if (toolName === "shell") {
    let cmd: string | undefined;
    try {
      cmd = (JSON.parse(argsJson) as { command?: string }).command;
    } catch {
      cmd = undefined;
    }
    if (cmd === undefined) return false;
    // A git verb at COMMAND position (start of the command or after a
    // separator like `;`/`&&`/`|`/`(`), allowing global flags with or
    // without values (`git -c x=y merge`, `git -C path commit`) — but NOT a
    // mere mention inside another command (`echo git commit`).
    return /(?:^|[|;&,(]\s*)git\s+(?:-(?:C|c)\s+\S+\s+|-\S+\s+)*(?:commit|merge|checkout|branch)\b/.test(
      cmd,
    );
  }
  if (toolName === "create_plan") {
    try {
      const branch = (JSON.parse(argsJson) as { branch?: unknown }).branch;
      return typeof branch === "string" && branch.trim().length > 0;
    } catch {
      return false;
    }
  }
  return false;
}

/**
 * Compact date label for the graph ("Aug 20") from a unix-seconds
 * timestamp — the DAG rows show the short form; the detail row shows the
 * full locale string.
 */
export function shortDate(timestampSec: number): string {
  return new Date(timestampSec * 1000).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}
