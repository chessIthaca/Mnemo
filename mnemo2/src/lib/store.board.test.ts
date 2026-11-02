import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BoardTransport } from "./boardEngine";
import type { BoardPlan } from "./boardState";
import type { TeamTransport } from "./teamEngine";
import type { TeamWorkerRecord } from "./teamState";

const hoisted = vi.hoisted(() => ({
  team: null as TeamTransport | null,
  board: null as BoardTransport | null,
}));

vi.mock("./teamEngine", () => ({ createTeamTransport: () => hoisted.team }));
vi.mock("./boardEngine", () => ({ createBoardTransport: () => hoisted.board }));
vi.mock("./leadEngine", () => ({ createLeadTransport: () => null }));
vi.mock("./projectEngine", () => ({
  createProjectTransport: () => ({
    attach: async () => ({ conflict_pid: null, conflict_started_at: null }),
    preferences: async () => ({
      workbench_open: false,
      workbench_tab: "browser",
      workbench_width: 480,
      browser_url: "about:blank",
      diff_worker_id: null,
    }),
  }),
}));
vi.mock("./runtime", () => ({
  isTauriRuntime: () => true,
  probeMnemo2Host: async () => true,
}));

import { actions, app } from "./store";
import { fixtureState } from "../test/fixtures";

function plan(overrides: Partial<BoardPlan> = {}): BoardPlan {
  return {
    id: "plan-aaaa-1111",
    title: "Ship it",
    goal: "g",
    kind: "feature",
    status: "open",
    closed_at_ms: null,
    report_path: null,
    steps: [{ number: 1, header: null, text: "Do", done: false, workers: [] }],
    ...overrides,
  };
}

function worker(status: string, planStep: number | null = 0): TeamWorkerRecord {
  return {
    id: "ana",
    name: "Ana",
    brief: "task",
    model: null,
    model_id: null,
    branch: "b",
    worktree: "w",
    status,
    generation: 1,
    transcript: { messages: [] },
    pending: null,
    used_tokens: 0,
    max_tokens: 1,
    total_tokens: 0,
    prompt_tokens: 0,
    completion_tokens: 0,
    cached_tokens: 0,
    requests: 0,
    cost_micros: null,
    created_at_ms: 1,
    elapsed_ms: 1,
    turns: 0,
    last_activity_ms: 1,
    checks: [],
    findings: [],
    review: null,
    fixer_finding_id: null,
    plan_id: "plan-aaaa-1111",
    plan_step: planStep,
    merge: null,
    error: null,
  } as TeamWorkerRecord;
}

describe("board store slice", () => {
  let changed: () => void = () => {};
  let plans: BoardPlan[];
  let record: TeamWorkerRecord;
  let team: TeamTransport;
  let board: BoardTransport;

  beforeEach(() => {
    plans = [plan()];
    record = worker("working");
    team = {
      snapshot: vi.fn(async () => ({ workers: [record], slots: 3, active: 1 })),
      messages: vi.fn(async () => []),
      onEvent: vi.fn(async () => () => {}),
      stopAll: vi.fn(async () => {}),
    } as unknown as TeamTransport;
    board = {
      snapshot: vi.fn(async () => ({ plans })),
      document: vi.fn(async () => "# Plan: fixture\n"),
      onChanged: vi.fn(async (handler: () => void) => {
        changed = handler;
        return () => {};
      }),
    };

    hoisted.team = team;
    hoisted.board = board;
    app.set(fixtureState());
  });

  afterEach(async () => {
    await actions.closeProject();
    hoisted.team = null;
    hoisted.board = null;
  });

  it("loads the board on open and stamps workers with their plan", async () => {
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).board).toHaveLength(1));
    expect(get(app).board[0].title).toBe("Ship it");
    expect(get(app).workers[0]).toMatchObject({ planId: "plan-aaaa-1111", planStep: 0 });
  });

  it("refetches on the board-changed event", async () => {
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).board).toHaveLength(1));
    plans = [plan(), plan({ id: "second", title: "Second" })];
    changed();
    await vi.waitFor(() => expect(get(app).board).toHaveLength(2));
  });

  it("refetches when a team sync sees a worker status change", async () => {
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).board).toHaveLength(1));
    const fetches = vi.mocked(board.snapshot).mock.calls.length;
    plans = [plan({ steps: [{ number: 1, header: null, text: "Do", done: true, workers: [] }] })];
    record = worker("merged");
    await actions.syncTeam();
    await vi.waitFor(() => expect(get(app).board[0].steps[0].done).toBe(true));
    expect(vi.mocked(board.snapshot).mock.calls.length).toBeGreaterThan(fetches);
  });

  it("clears the board when the project closes", async () => {
    record = worker("merged");
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).board).toHaveLength(1));
    await actions.closeProject();
    expect(get(app).board).toEqual([]);
  });
});
