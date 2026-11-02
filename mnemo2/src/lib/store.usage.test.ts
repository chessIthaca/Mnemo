import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LeadTransport } from "./leadEngine";
import type { TeamTransport } from "./teamEngine";
import type { LeadEvent, LeadSnapshot } from "./types";
import type { ProjectUsageView, TeamDeleteOutcome, TeamEvent, TeamSnapshot } from "./teamState";

const harness = vi.hoisted(() => ({
  team: null as TeamTransport | null,
  lead: null as LeadTransport | null,
}));

vi.mock("./teamEngine", () => ({
  createTeamTransport: () => harness.team,
}));
vi.mock("./leadEngine", () => ({
  createLeadTransport: () => harness.lead,
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

function usageView(overrides: Partial<ProjectUsageView> = {}): ProjectUsageView {
  return {
    input_tokens: 0,
    cached_tokens: 0,
    output_tokens: 0,
    cost_micros: 0,
    unpriced_models: [],
    ...overrides,
  };
}

function makeTeam(initial: ProjectUsageView) {
  let usage = initial;
  const handlers: ((event: TeamEvent) => void)[] = [];
  const snapshot = vi.fn(async (): Promise<TeamSnapshot> => ({
    workers: [],
    slots: 2,
    active: 0,
    usage,
  }));
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
    setSlots: vi.fn(async (): Promise<TeamSnapshot> => ({
      workers: [],
      slots: 2,
      active: 0,
      usage,
    })),
    deletePreview: vi.fn(async () => []),
    deleteWorkers: vi.fn(async (): Promise<TeamDeleteOutcome> => ({
      results: [],
      snapshot: {
        workers: [],
        slots: 2,
        active: 0,
        usage,
      },
    })),
    messages: vi.fn(async () => []),
    onEvent: vi.fn(async (handler: (event: TeamEvent) => void) => {
      handlers.push(handler);
      return () => {};
    }),
  };
  return {
    transport,
    snapshot,
    setUsage(next: ProjectUsageView) {
      usage = next;
    },
    emit(event: TeamEvent) {
      for (const handler of handlers) handler(event);
    },
  };
}

function leadSnapshot(generation = 1): LeadSnapshot {
  return {
    generation,
    model: "test/scripted-a",
    model_id: "scripted-a",
    models: [
      { endpoint: "test", model: "scripted-a", spec: "test/scripted-a", is_default: true },
    ],
    default_model: "test/scripted-a",
    transcript: { messages: [] },
    used_tokens: 0,
    max_tokens: 1000,
    pending: null,
    busy: false,
  };
}

function makeLead() {
  const handlers: ((event: LeadEvent) => void)[] = [];
  let current = leadSnapshot();
  const transport: LeadTransport = {
    probe: vi.fn(async () => true),
    open: vi.fn(async () => current),
    snapshot: vi.fn(async () => current),
    prompt: vi.fn(async () => {}),
    interrupt: vi.fn(async () => {}),
    compact: vi.fn(async () => {}),
    setModel: vi.fn(async () => current),
    approve: vi.fn(async () => {}),
    answer: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    onEvent: vi.fn(async (handler: (event: LeadEvent) => void) => {
      handlers.push(handler);
      return () => {};
    }),
  };
  return {
    transport,
    emit(event: LeadEvent) {
      for (const handler of handlers) handler(event);
    },
  };
}

describe("durable project usage", () => {
  beforeEach(() => {
    harness.team = null;
    harness.lead = null;
    app.set(fixtureState());
  });

  it("restores the persisted usage aggregate when a project opens", async () => {
    const team = makeTeam(
      usageView({
        input_tokens: 900,
        cached_tokens: 300,
        output_tokens: 120,
        cost_micros: 1_234_567,
        unpriced_models: ["mystery-model"],
      }),
    );
    harness.team = team.transport;

    await actions.openProject("mnemo");

    // No workers are live, yet the project's durable spend is still shown:
    // worker deletion no longer erases its contribution.
    expect(get(app).projectUsage).toEqual({
      inputTokens: 900,
      cachedTokens: 300,
      outputTokens: 120,
      costMicros: 1_234_567,
      unpricedModels: ["mystery-model"],
    });
  });

  it("refreshes the aggregate after a worker usage event", async () => {
    const team = makeTeam(usageView({ cost_micros: 1_000_000 }));
    harness.team = team.transport;
    await actions.openProject("mnemo");
    expect(get(app).projectUsage.costMicros).toBe(1_000_000);

    team.setUsage(
      usageView({ input_tokens: 40, output_tokens: 10, cost_micros: 2_400_000 }),
    );
    team.emit({
      worker_id: "w1",
      generation: 1,
      event: { agent: 1, event: { kind: "usage", prompt_tokens: 40, completion_tokens: 10 } },
    });

    await vi.waitFor(() => expect(get(app).projectUsage.costMicros).toBe(2_400_000));
    expect(get(app).projectUsage.inputTokens).toBe(40);
    expect(get(app).projectUsage.outputTokens).toBe(10);
  });

  it("refreshes the aggregate after the lead records usage", async () => {
    const team = makeTeam(usageView({ cost_micros: 500_000 }));
    const lead = makeLead();
    harness.team = team.transport;
    harness.lead = lead.transport;
    await actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).leadRuntime.status).toBe("ready"));

    team.setUsage(usageView({ input_tokens: 7, output_tokens: 3, cost_micros: 750_000 }));
    lead.emit({
      generation: 1,
      event: {
        agent: 1,
        event: { kind: "usage", prompt_tokens: 7, completion_tokens: 3, cached_tokens: 0 },
      },
    });

    await vi.waitFor(() => expect(get(app).projectUsage.costMicros).toBe(750_000));
  });

  it("drops a late team snapshot after the human switches projects", async () => {
    const team = makeTeam(usageView({ cost_micros: 1_000_000 }));
    harness.team = team.transport;
    await actions.openProject("mnemo");
    expect(get(app).projectUsage.costMicros).toBe(1_000_000);

    let release: (snapshot: TeamSnapshot) => void = () => {};
    team.transport.snapshot = vi.fn(
      () =>
        new Promise<TeamSnapshot>((resolve) => {
          release = resolve;
        }),
    );
    const pending = actions.syncTeam();

    // The human opens a different project while the old request is in flight.
    app.update((state) => ({ ...state, projectId: "garden-game" }));
    release({
      workers: [],
      slots: 2,
      active: 0,
      usage: usageView({ cost_micros: 9_000_000 }),
    });
    await pending;

    expect(get(app).projectId).toBe("garden-game");
    expect(get(app).projectUsage.costMicros).toBe(1_000_000);
  });
});
