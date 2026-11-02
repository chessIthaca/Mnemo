import { describe, expect, it } from "vitest";
import {
  boardFromSnapshot,
  planProgress,
  planTitle,
  workerPlanLabel,
  type BoardPlan,
} from "./boardState";

function boardPlan(overrides: Partial<BoardPlan> = {}): BoardPlan {
  return {
    id: "abcdef1234567890",
    title: "Ship the board",
    goal: "Show plans",
    kind: "feature",
    status: "open",
    closed_at_ms: null,
    report_path: null,
    steps: [
      { number: 1, header: "Engine", text: "Build it", done: true, workers: [] },
      { number: 2, header: null, text: "Wire the UI", done: false, workers: [] },
    ],
    ...overrides,
  };
}

describe("boardState", () => {
  it("orders open plans first and closed plans newest first", () => {
    const board = boardFromSnapshot({
      plans: [
        boardPlan({ id: "old", status: "closed", closed_at_ms: 10 }),
        boardPlan({ id: "open-a" }),
        boardPlan({ id: "new", status: "abandoned", closed_at_ms: 20 }),
      ],
    });
    expect(board.map((plan) => plan.id)).toEqual(["open-a", "new", "old"]);
  });

  it("treats a missing snapshot as an empty board", () => {
    expect(boardFromSnapshot(null)).toEqual([]);
  });

  it("counts done steps", () => {
    expect(planProgress(boardPlan())).toEqual({ done: 1, total: 2 });
  });

  it("labels a worker with the plan title and 1-based step", () => {
    const board = [boardPlan()];
    expect(workerPlanLabel(board, { planId: "abcdef1234567890", planStep: 1 }))
      .toBe("Ship the board · step 2");
    expect(workerPlanLabel(board, { planId: null, planStep: null })).toBeNull();
  });

  it("falls back to the first 8 id characters when the board lacks the plan", () => {
    expect(planTitle([], "abcdef1234567890")).toBe("abcdef12");
    expect(workerPlanLabel([], { planId: "abcdef1234567890", planStep: 0 }))
      .toBe("abcdef12 · step 1");
  });
});
