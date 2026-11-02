import { afterEach, describe, expect, it, vi } from "vitest";
import {
  canMoveMain,
  createGitTransport,
  distanceLabel,
  moveQuestion,
  seedGitPreview,
  type GitBranch,
} from "./git";

vi.mock("./runtime", () => ({ isTauriRuntime: vi.fn(() => false) }));

const branch = (name: string, ahead: number, behind: number, current = false): GitBranch => ({
  name,
  ahead,
  behind,
  current,
});

describe("canMoveMain", () => {
  it("allows a plain fast-forward of a branch that is ahead only", () => {
    expect(canMoveMain(branch("work", 2, 0), null)).toBe(true);
  });

  it("refuses a branch behind main, main itself, and any checkout of main", () => {
    expect(canMoveMain(branch("work", 1, 1), null)).toBe(false);
    expect(canMoveMain(branch("work", 0, 2), null)).toBe(false);
    expect(canMoveMain(branch("work", 0, 0), null)).toBe(false);
    expect(canMoveMain(branch("main", 2, 0), null)).toBe(false);
    expect(canMoveMain(branch("work", 2, 0), "C:/project/.worktrees/main")).toBe(false);
  });

  it("allows the current branch, which is where \"move main here\" points", () => {
    expect(canMoveMain(branch("work", 2, 0, true), null)).toBe(true);
  });
});

describe("moveQuestion", () => {
  it("names the commit count the fast-forward adds and that nothing is pushed", () => {
    expect(moveQuestion(branch("work", 2, 0))).toBe(
      "Move main to work? This fast-forwards main by 2 commits. Nothing is pushed.",
    );
    expect(moveQuestion(branch("hotfix", 1, 0))).toContain("by 1 commit.");
  });
});

describe("distanceLabel", () => {
  it("reads a branch's distance from main", () => {
    expect(distanceLabel(branch("work", 0, 0))).toBe("level with main");
    expect(distanceLabel(branch("work", 2, 0))).toBe("2 ahead main");
    expect(distanceLabel(branch("work", 0, 3))).toBe("3 behind main");
    expect(distanceLabel(branch("work", 2, 3))).toBe("2 ahead, 3 behind main");
  });
});

describe("createGitTransport", () => {
  afterEach(() => {
    seedGitPreview(null);
  });

  it("is null outside the desktop app, so the tab can say so", () => {
    expect(createGitTransport()).toBeNull();
  });

  it("uses the preview seed when one is installed", async () => {
    seedGitPreview({
      overview: async () => ({
        branch: "work",
        main: "abc1234",
        main_worktree: null,
        main_dirty: false,
        branches: [branch("work", 2, 0, true)],
        commits: [],
        truncated: false,
      }),
      moveMain: async (name) => ({ branch: name, main: "def5678", commits: 2 }),
    });
    const transport = createGitTransport();
    expect(transport).not.toBeNull();
    expect((await transport?.overview())?.branch).toBe("work");
    expect(await transport?.moveMain("work")).toEqual({
      branch: "work",
      main: "def5678",
      commits: 2,
    });
  });
});
