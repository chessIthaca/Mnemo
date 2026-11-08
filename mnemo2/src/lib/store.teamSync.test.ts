import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TeamTransport } from "./teamEngine";
import {
  emptyProjectUsageView,
  type TeamDeleteOutcome,
  type TeamSnapshot,
  type TeamWorkerRecord,
} from "./teamState";

const harness = vi.hoisted(() => ({
  team: null as TeamTransport | null,
}));

vi.mock("./teamEngine", () => ({
  createTeamTransport: () => harness.team,
}));
vi.mock("./leadEngine", () => ({
  createLeadTransport: () => null,
}));
vi.mock("./projectEngine", () => ({
  createProjectTransport: () => ({
    attach: async () => ({ conflict_pid: null, conflict_started_at: null }),
    preferences: async () => ({
      workbench_open: false,
      workbench_tab: "browser",
      workbench_width: 480,
      browser_url: "about:blank",
      diff_worker_id: null,
      board_width: null,
    }),
  }),
}));
vi.mock("./runtime", () => ({
  isTauriRuntime: () => true,
  probeMnemo2Host: async () => true,
}));

import { actions, app } from "./store";
import { fixtureState } from "../test/fixtures";

function workerRecord(id: string, texts: string[]): TeamWorkerRecord {
  return {
    id,
    name: `Worker ${id}`,
    brief: "do the task",
    model: "test/scripted-a",
    model_id: "scripted-a",
    branch: `wt/agent-${id}`,
    worktree: `C:/repo/.worktrees/agent-${id}`,
    status: "working",
    generation: 1,
    transcript: {
      messages: texts.map((content) => ({ role: "assistant", content })),
    },
    pending: null,
    used_tokens: 10,
    max_tokens: 100,
    total_tokens: 10,
    prompt_tokens: 8,
    completion_tokens: 2,
    cached_tokens: 3,
    requests: 2,
    cost_micros: 500_000,
    created_at_ms: 1_787_399_996_000,
    elapsed_ms: 4000,
    turns: 2,
    last_activity_ms: 1_787_400_000_000,
    checks: [],
    findings: [],
    review: null,
    fixer_finding_id: null,
    merge: null,
    error: null,
  };
}

function snapshotOf(workers: TeamWorkerRecord[], revs: Record<string, number>): TeamSnapshot {
  return {
    workers,
    slots: 2,
    active: workers.length,
    usage: emptyProjectUsageView(),
    transcript_revs: revs,
  };
}

function makeTeam() {
  let next = snapshotOf([workerRecord("w1", ["first message"])], { w1: 7 });
  const snapshot = vi.fn(
    async (_known?: Record<string, number>): Promise<TeamSnapshot> => next,
  );
  const transport: TeamTransport = {
    snapshot,
    dispatch: vi.fn(),
    message: vi.fn(),
    pause: vi.fn(),
    interrupt: vi.fn(),
    stop: vi.fn(),
    resume: vi.fn(),
    setModel: vi.fn(),
    compact: vi.fn(),
    review: vi.fn(),
    recordCheck: vi.fn(),
    resolveFinding: vi.fn(),
    reviewFix: vi.fn(),
    routeFix: vi.fn(),
    merge: vi.fn(),
    revertMerge: vi.fn(),
    stopAll: vi.fn(),
    setSlots: vi.fn(async (): Promise<TeamSnapshot> => next),
    deletePreview: vi.fn(async () => []),
    deleteWorkers: vi.fn(
      async (): Promise<TeamDeleteOutcome> => ({ results: [], snapshot: next }),
    ),
    messages: vi.fn(async () => []),
    onEvent: vi.fn(async () => () => {}),
  };
  return {
    transport,
    snapshot,
    setNext(value: TeamSnapshot) {
      next = value;
    },
  };
}

describe("team sync transcript revisions", () => {
  beforeEach(() => {
    harness.team = null;
    app.set(fixtureState());
  });

  it("sends back the revisions it holds and restores omitted transcripts", async () => {
    const team = makeTeam();
    harness.team = team.transport;

    // The first sync holds nothing, so it asks for every transcript and their
    // revisions; the startup path then syncs again with the revisions it got.
    await actions.openProject("mnemo");
    expect(team.snapshot).toHaveBeenNthCalledWith(1, {});
    expect(team.snapshot).toHaveBeenLastCalledWith({ w1: 7 });

    const thread = get(app).workers[0].messages;
    expect(thread).toHaveLength(1);
    expect(thread[0]).toMatchObject({ body: "first message" });

    // The host now leaves the transcript out: the cached one must come back
    // and the thread must not change.
    team.setNext({
      ...snapshotOf([workerRecord("w1", [])], { w1: 7 }),
      omitted_transcripts: ["w1"],
    });
    const calls = team.snapshot.mock.calls.length;
    await actions.syncTeam();

    expect(team.snapshot).toHaveBeenCalledTimes(calls + 1);
    expect(team.snapshot).toHaveBeenLastCalledWith({ w1: 7 });
    expect(get(app).workers[0].messages).toEqual(thread);
  });

  it("drops the cache and refetches everything when a revision no longer matches", async () => {
    const team = makeTeam();
    harness.team = team.transport;
    await actions.openProject("mnemo");

    // A left-out transcript the window cannot rebuild (a stale revision) makes
    // the store drop its cache and schedule a full resync instead of showing a
    // truncated thread.
    team.setNext({
      ...snapshotOf([workerRecord("w1", [])], { w1: 8 }),
      omitted_transcripts: ["w1"],
    });
    await actions.syncTeam();

    expect(get(app).workers[0].messages).toHaveLength(1);

    // The scheduled resync asks for every transcript again and heals the store.
    team.setNext(snapshotOf([workerRecord("w1", ["first message"])], { w1: 9 }));
    await vi.waitFor(() => expect(team.snapshot).toHaveBeenLastCalledWith({}));
    expect(get(app).workers[0].messages).toHaveLength(1);
    expect(get(app).workers[0].messages[0]).toMatchObject({ body: "first message" });
  });
});
