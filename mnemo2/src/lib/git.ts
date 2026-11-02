import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

/** One local branch and its distance from `main`. */
export interface GitBranch {
  /** The short branch name, exactly as Git spells it. */
  name: string;
  /** True for the branch checked out in the project root. */
  current: boolean;
  /** Commits this branch has that `main` does not — what moving `main` gains. */
  ahead: number;
  /** Commits `main` has that this branch does not — nonzero blocks a fast-forward. */
  behind: number;
}

/** One row of the commit lane. */
export interface GitCommit {
  /** The full object id. */
  id: string;
  /** The abbreviated id the row shows. */
  short: string;
  /** The commit message's first line. */
  summary: string;
  /** The author's name. */
  author: string;
  /** The commit date in Git's relative form (`2 hours ago`). */
  date: string;
  /** The branches and tags that point at this commit. */
  refs: string[];
}

/** Everything the Git tab renders in one read. */
export interface GitOverview {
  /** The checked-out branch, or `null` when `HEAD` is detached. */
  branch: string | null;
  /** The `main` tip's abbreviated id, or `null` when there is no `main`. */
  main: string | null;
  /** The worktree that has `main` checked out, if any; moving `main` is refused then. */
  main_worktree: string | null;
  /** True when that worktree has uncommitted changes. */
  main_dirty: boolean;
  /** Local branches: the current one first, then `main`, then by name. */
  branches: GitBranch[];
  /** The newest commits, newest first. */
  commits: GitCommit[];
  /** True when the backend cut the lane short at 50 commits. */
  truncated: boolean;
}

/** What a completed move of `main` reports back. */
export interface GitMove {
  /** The branch `main` now points at. */
  branch: string;
  /** The new `main` tip, abbreviated. */
  main: string;
  /** How many commits `main` gained. */
  commits: number;
}

/** The Git tab's three backend reads and its one write. */
export interface GitTransport {
  overview(): Promise<GitOverview>;
  moveMain(branch: string): Promise<GitMove>;
}

/** The dev-only preview's stand-in transport; never set in production. */
let previewSeed: GitTransport | null = null;

/**
 * Point the Git tab at fixture data for the dev-only preview page
 * (`npm run check:no-mock` keeps this out of the production entry).
 */
export function seedGitPreview(transport: GitTransport | null): void {
  previewSeed = transport;
}

/** Build the Git tab's transport, or `null` outside the desktop app. */
export function createGitTransport(): GitTransport | null {
  if (previewSeed) return previewSeed;
  if (!isTauriRuntime()) return null;
  return {
    overview: () => invoke<GitOverview>("git_overview"),
    moveMain: (branch) => invoke<GitMove>("git_move_main", { branch }),
  };
}

/**
 * True when `main` can fast-forward onto `branch`: `main` is not checked out
 * anywhere, the branch carries commits `main` lacks, and it has nothing `main`
 * would lose. The current branch is the normal target — that is "move main
 * here".
 */
export function canMoveMain(branch: GitBranch, mainWorktree: string | null): boolean {
  return (
    mainWorktree === null &&
    branch.name !== "main" &&
    branch.ahead > 0 &&
    branch.behind === 0
  );
}

/**
 * The confirmation the tab asks before its one write. It names the commit count
 * the move adds and states plainly that nothing leaves the machine.
 */
export function moveQuestion(branch: GitBranch): string {
  const count = branch.ahead === 1 ? "1 commit" : `${branch.ahead} commits`;
  return `Move main to ${branch.name}? This fast-forwards main by ${count}. Nothing is pushed.`;
}

/** How far a branch sits from `main`, as the branch row reads it. */
export function distanceLabel(branch: GitBranch): string {
  if (branch.ahead === 0 && branch.behind === 0) return "level with main";
  const parts: string[] = [];
  if (branch.ahead > 0) parts.push(`${branch.ahead} ahead`);
  if (branch.behind > 0) parts.push(`${branch.behind} behind`);
  return `${parts.join(", ")} main`;
}
