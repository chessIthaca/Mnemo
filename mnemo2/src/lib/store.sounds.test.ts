import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TeamTransport } from "./teamEngine";
import { emptyProjectUsageView, type TeamEvent, type TeamWorkerRecord } from "./teamState";
import type { LeadEvent, LeadSnapshot } from "./types";

/**
 * The store rings the notification sounds at the four moments the Appearance
 * page describes — a finished lead turn, a finished worker turn, a needs-you
 * prompt (lead or worker) and a turn that stopped with errors — and stays
 * silent for everything else. `notifySound` is the store's single door to the
 * sound module (its gating and debounce are pinned in `sounds.test.ts`); this
 * file proves the store calls it at the right moment, with the right sound.
 *
 * The host is a scripted fake: a native lead whose events the test pushes, and
 * a team whose snapshot the test swaps between pushes.
 */
const host = vi.hoisted(() => ({
  notified: [] as string[],
  leadEmit: null as null | ((event: LeadEvent) => void),
  teamEmit: null as null | ((event: TeamEvent) => void),
  workers: [] as TeamWorkerRecord[],
}));

vi.mock("./sounds", async (importOriginal) => {
  const real = await importOriginal<typeof import("./sounds")>();
  return {
    ...real,
    notifySound: (kind: string) => {
      host.notified.push(kind);
      return true;
    },
    // The startup read is the Appearance page's business; no IPC here.
    loadSoundToggles: async () => null,
  };
});
vi.mock("./runtime", () => ({
  isTauriRuntime: () => true,
  probeMnemo2Host: async () => true,
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
vi.mock("./leadEngine", () => ({
  createLeadTransport: () => ({
    probe: async () => true,
    open: async () => leadSnapshot,
    snapshot: async () => leadSnapshot,
    close: async () => undefined,
    onEvent: async (handler: (event: LeadEvent) => void) => {
      host.leadEmit = handler;
      return () => undefined;
    },
  }),
}));
vi.mock("./teamEngine", () => ({
  createTeamTransport: (): TeamTransport => ({
    snapshot: vi.fn(async () => ({
      workers: host.workers,
      slots: 3,
      active: 1,
      usage: emptyProjectUsageView(),
    })),
    messages: vi.fn(async () => []),
    onEvent: vi.fn(async (handler: (event: TeamEvent) => void) => {
      host.teamEmit = handler;
      return () => undefined;
    }),
    dispatch: vi.fn(),
    message: vi.fn(),
    pause: vi.fn(),
    interrupt: vi.fn(),
    stop: vi.fn(),
    resume: vi.fn(),
    setModel: vi.fn(),
    review: vi.fn(),
    recordCheck: vi.fn(),
    resolveFinding: vi.fn(),
    reviewFix: vi.fn(),
    routeFix: vi.fn(),
    merge: vi.fn(),
    revertMerge: vi.fn(),
    stopAll: vi.fn(),
    setSlots: vi.fn(),
    deletePreview: vi.fn(),
    deleteWorkers: vi.fn(),
    compact: vi.fn(),
  }),
}));

import { actions, app } from "./store";
import { fixtureState } from "../test/fixtures";

/** The lead snapshot the fake host opens with (one native generation). */
const leadSnapshot: LeadSnapshot = {
  generation: 1,
  model: "test/scripted-a",
  model_id: "scripted-a",
  models: [],
  default_model: "test/scripted-a",
  transcript: { messages: [] },
  used_tokens: 0,
  max_tokens: 1_000,
  pending: null,
  busy: false,
};

/** One host worker record, mid-turn unless overridden. */
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
    max_tokens: 1_000,
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

/** One envelope the lead host pushes on its event channel. */
function leadEvent(kind: string, extra: Record<string, unknown> = {}, generation = 1): LeadEvent {
  return { generation, event: { agent: 1, event: { kind, ...extra } } };
}

/** One envelope the team host pushes on its event channel. */
function teamEvent(kind = "finished"): TeamEvent {
  return { worker_id: "ana", generation: 1, event: { agent: 1, event: { kind } } };
}

/** Open the project with the given worker snapshot and the lead attached. */
async function open(): Promise<void> {
  await actions.openProject("mnemo");
  expect(get(app).projectId).toBe("mnemo");
  expect(get(app).leadRuntime.mode).toBe("native");
  expect(host.leadEmit).not.toBeNull();
  expect(host.teamEmit).not.toBeNull();
}

/** Swap the host's worker snapshot and force one resync through the event pump. */
async function pushWorkers(...workers: TeamWorkerRecord[]): Promise<void> {
  host.workers = workers;
  host.teamEmit?.(teamEvent());
  // The store debounces its team sync by 120 ms.
  await vi.waitFor(() => expect(get(app).workers[0]?.status).toBe(workers[0]?.status), { timeout: 2_000 });
}

describe("notification sounds from the store", () => {
  beforeEach(() => {
    host.notified.length = 0;
    host.leadEmit = null;
    host.teamEmit = null;
    host.workers = [nativeWorker()];
    app.set(fixtureState());
  });

  afterEach(async () => {
    await actions.closeProject();
  });

  it("rings the completion chime for a finished lead turn", async () => {
    await open();
    host.leadEmit?.(leadEvent("finished"));
    expect(host.notified).toEqual(["complete"]);
  });

  it("rings the needs-you ping for a lead approval request or question", async () => {
    await open();
    host.leadEmit?.(leadEvent("approval_request", { request_id: "req-1", tool_name: "shell" }));
    host.leadEmit?.(leadEvent("user_question", { question_id: "q-1", question: "Which branch?" }));
    expect(host.notified).toEqual(["input_needed", "input_needed"]);
  });

  it("rings the error tone for a lead turn that stopped, but not for a retry", async () => {
    await open();
    host.leadEmit?.(leadEvent("error", { error: "provider down", retrying: true }));
    expect(host.notified).toEqual([]);
    host.leadEmit?.(leadEvent("error", { error: "provider down" }));
    expect(host.notified).toEqual(["stopped_errors"]);
  });

  it("ignores lead events from a superseded generation", async () => {
    await open();
    host.leadEmit?.(leadEvent("finished", {}, 7));
    expect(host.notified).toEqual([]);
  });

  it("rings for a worker that finishes, waits for the human, or fails", async () => {
    await open();
    await pushWorkers(nativeWorker({ status: "completed" }));
    expect(host.notified).toEqual(["complete"]);

    await pushWorkers(nativeWorker({ status: "working" }));
    await pushWorkers(nativeWorker({ status: "needs_you" }));
    expect(host.notified).toEqual(["complete", "input_needed"]);

    await pushWorkers(nativeWorker({ status: "working" }));
    await pushWorkers(nativeWorker({ status: "failed", error: "check failed" }));
    expect(host.notified).toEqual(["complete", "input_needed", "stopped_errors"]);
  });

  it("stays silent when a snapshot only restates a terminal status", async () => {
    await open();
    await pushWorkers(nativeWorker({ status: "completed" }));
    expect(host.notified).toEqual(["complete"]);
    // A later snapshot repeats the same status: nothing new to announce.
    await pushWorkers(nativeWorker({ status: "completed" }));
    expect(host.notified).toEqual(["complete"]);
  });

  it("stays silent for human-stopped workers", async () => {
    await open();
    await pushWorkers(nativeWorker({ status: "stopped" }));
    expect(host.notified).toEqual([]);
  });
});
