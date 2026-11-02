import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TeamTransport } from "./teamEngine";
import { emptyProjectUsageView, type TeamEvent, type TeamWorkerRecord } from "./teamState";

const team = vi.hoisted(() => ({
  transport: null as TeamTransport | null,
}));

vi.mock("./teamEngine", () => ({
  createTeamTransport: () => team.transport,
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

/** A native worker record straight from the host, mid-turn. */
function nativeWorker(overrides: Partial<TeamWorkerRecord> = {}): TeamWorkerRecord {
  return {
    id: "ana",
    name: "Ana",
    brief: "Implement the real task",
    model: "test/scripted-a",
    model_id: "scripted-a",
    branch: "wt/agent-ana",
    worktree: "C:/repo/.worktrees/agent-ana",
    status: "working",
    generation: 1,
    transcript: { messages: [] },
    pending: null,
    used_tokens: 123,
    max_tokens: 1000,
    total_tokens: 123,
    prompt_tokens: 100,
    completion_tokens: 23,
    cached_tokens: 10,
    requests: 4,
    cost_micros: 500_000,
    created_at_ms: 1_787_399_935_000,
    elapsed_ms: 65_000,
    turns: 4,
    last_activity_ms: 1_787_400_000_000,
    checks: [],
    findings: [],
    review: null,
    fixer_finding_id: null,
    merge: null,
    error: null,
    ...overrides,
  };
}

/** The team transport with the host's event pump wired to the returned `emit`. */
function makeTransport(record: TeamWorkerRecord): { transport: TeamTransport; emit: (event: TeamEvent) => void } {
  let listener: ((event: TeamEvent) => void) | null = null;
  const transport: TeamTransport = {
    snapshot: vi.fn(async () => ({ workers: [record], slots: 3, active: 1, usage: emptyProjectUsageView() })),
    dispatch: vi.fn(async () => record),
    message: vi.fn(async () => record),
    pause: vi.fn(async () => record),
    interrupt: vi.fn(async () => record),
    stop: vi.fn(async () => record),
    resume: vi.fn(async () => record),
    setModel: vi.fn(async () => record),
    review: vi.fn(async () => record),
    recordCheck: vi.fn(async () => record),
    resolveFinding: vi.fn(async () => record),
    reviewFix: vi.fn(async () => record),
    routeFix: vi.fn(async () => record),
    merge: vi.fn(async () => record),
    revertMerge: vi.fn(async () => record),
    stopAll: vi.fn(async () => {}),
    setSlots: vi.fn(async () => ({ workers: [record], slots: 3, active: 1, usage: emptyProjectUsageView() })),
    deletePreview: vi.fn(async () => []),
    deleteWorkers: vi.fn(async () => ({
      results: [],
      snapshot: { workers: [], slots: 3, active: 0, usage: emptyProjectUsageView() },
    })),
    compact: vi.fn(async () => record),
    messages: vi.fn(async () => []),
    onEvent: vi.fn(async (handler: (event: TeamEvent) => void) => {
      listener = handler;
      return () => {};
    }),
  };
  const emit = (event: TeamEvent) => {
    // Failing loudly here means the store never subscribed, so a silent
    // no-op can never let the activity assertions below pass vacuously.
    if (!listener) throw new Error("the store never subscribed to team events");
    listener(event);
  };
  return { transport, emit };
}

/** One streamed worker event, the way the host serializes it. */
const stream = (kind: string, extra: Record<string, unknown> = {}): TeamEvent => ({
  worker_id: "ana",
  generation: 1,
  event: { agent: 1, event: { kind, ...extra } },
});

async function openTeam(transport: TeamTransport) {
  team.transport = transport;
  // Awaiting the whole open waits out its internal syncs, so the workers in
  // state are this transport's snapshot — not a stale one from a previous open.
  await actions.openProject("mnemo");
  expect(get(app).workers).toHaveLength(1);
}

describe("worker live activity in the store", () => {
  beforeEach(() => {
    team.transport = null;
    app.set(fixtureState());
  });

  afterEach(async () => {
    // Awaited: an un-awaited close keeps clearing workers into the next test.
    await actions.closeProject();
    team.transport = null;
  });

  it("exposes createdAtMs and a `starting` activity for a working worker that has not streamed yet", async () => {
    const harness = makeTransport(nativeWorker());
    await openTeam(harness.transport);

    expect(get(app).workers[0]).toMatchObject({
      createdAtMs: 1_787_399_935_000,
      activity: { phase: "starting", toolName: null, turnStartedAtMs: null },
    });
  });

  it("updates a row-only worker's phase from streamed events", async () => {
    const harness = makeTransport(nativeWorker());
    const emit = harness.emit;
    await openTeam(harness.transport);

    // The worker's thread is not selected or opened anywhere — the row still
    // tracks every streamed phase change as it happens.
    emit(stream("started"));
    expect(get(app).workers[0].activity?.phase).toBe("starting");
    expect(get(app).workers[0].activity?.turnStartedAtMs).not.toBeNull();

    emit(stream("reasoning_delta"));
    expect(get(app).workers[0].activity?.phase).toBe("thinking");

    emit(stream("text_delta"));
    expect(get(app).workers[0].activity?.phase).toBe("writing");

    emit(stream("tool_call_start", { name: "shell" }));
    expect(get(app).workers[0].activity).toMatchObject({ phase: "tool", toolName: "shell" });

    emit(stream("tool_output_delta"));
    expect(get(app).workers[0].activity?.phase).toBe("tool");

    emit(stream("tool_result"));
    expect(get(app).workers[0].activity?.phase).toBe("thinking");

    emit(stream("approval_request"));
    expect(get(app).workers[0].activity?.phase).toBe("waiting");

    emit(stream("finished"));
    expect(get(app).workers[0].activity).toBeNull();
  });

  it("waits on the user when the snapshot says the worker needs an answer", async () => {
    const harness = makeTransport(nativeWorker({ status: "needs_you" }));
    const emit = harness.emit;
    await openTeam(harness.transport);

    expect(get(app).workers[0].activity?.phase).toBe("waiting");

    // The streamed turn anchor survives the status fold — the thread's timer
    // keeps the turn it is waiting inside.
    emit(stream("started"));
    expect(get(app).workers[0].activity?.turnStartedAtMs).not.toBeNull();
    emit(stream("user_question"));
    expect(get(app).workers[0].activity?.phase).toBe("waiting");
    expect(get(app).workers[0].activity?.turnStartedAtMs).not.toBeNull();
  });

  it("keeps the live activity empty for workers that are not running", async () => {
    for (const status of ["completed", "failed", "idle", "paused"]) {
      // Re-opening reconnects the whole team, so each status gets its own snapshot.
      const harness = makeTransport(nativeWorker({ status: status as TeamWorkerRecord["status"] }));
      await openTeam(harness.transport);
      expect(get(app).workers[0].activity).toBeNull();
    }
  });
});