import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import TraceTab from "./TraceTab.svelte";
import { app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import type {
  TraceAttribution,
  TraceDetail,
  TraceListPage,
  TraceListQuery,
  TracePageTransport,
  TraceRow,
} from "../lib/trace";

const mocks = vi.hoisted(() => ({
  transport: null as TracePageTransport | null,
  rows: [] as TraceRow[],
  details: new Map<string, TraceDetail>(),
  queries: [] as TraceListQuery[],
  detailKeys: [] as string[],
  failList: 0,
}));

vi.mock("../lib/trace", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/trace")>();
  return { ...original, createTracePageTransport: () => mocks.transport };
});

/** 2026-11-02T14:00:00Z — a fixed minute so the clock output is stable. */
const NOW = Date.UTC(2026, 11, 2, 14, 0, 0);

function attribution(overrides: Partial<TraceAttribution> = {}): TraceAttribution {
  return {
    actor_kind: "lead",
    agent_id: "agent-lead",
    session_id: "session-1",
    agent_name: "Lead",
    worker_id: null,
    plan_id: null,
    plan_step: null,
    turn_id: "turn-4f2a",
    purpose: "turn",
    ...overrides,
  };
}

function row(overrides: Partial<TraceRow> = {}): TraceRow {
  return {
    id: 1,
    ts_ms: NOW,
    model: "lead-model",
    base_url: "https://api.example.invalid/v1",
    provider: "primary",
    http_status: 200,
    usage: { prompt: 1_200, completion: 240, reasoning: 0, cached: 900 },
    finish_reason: "stop",
    is_complete: true,
    version: 2,
    ttft_ms: 300,
    generation_ms: 900,
    reasoning_ms: null,
    connect_ms: null,
    tools_ms: null,
    prep_ms: null,
    compact_ms: null,
    backoff_ms: null,
    stall_ms: null,
    error: null,
    cancelled: false,
    response_truncated: false,
    response_evicted: false,
    guard_cut: null,
    run_id: "run-1",
    attribution: attribution(),
    cost_micros: 4_200,
    priced: true,
    retained: false,
    ...overrides,
  };
}

function detail(overrides: Partial<TraceDetail> = {}): TraceDetail {
  return {
    ...row(),
    request_json: {
      model: "lead-model",
      messages: [
        { role: "system", content: "You are the lead." },
        { role: "user", content: "Fix the failing test." },
      ],
    },
    response_raw: [
      'data: {"choices":[{"delta":{"reasoning_content":"Plan the fix first."}}]}',
      "",
    ].join("\n"),
    request_truncated: false,
    raw_tool_calls: null,
    ...overrides,
  };
}

function installTransport(): void {
  mocks.transport = {
    list: async (query: TraceListQuery) => {
      mocks.queries.push(query);
      if (mocks.queries.length <= mocks.failList) throw new Error("engine offline");
      const filtered = mocks.rows.filter((entry) => {
        if (query.agent === undefined) return true;
        if (query.agent === "lead") return entry.attribution?.actor_kind === "lead";
        return entry.attribution?.worker_id === query.agent || entry.attribution?.agent_id === query.agent;
      });
      const page: TraceListPage = {
        rows: filtered,
        total: filtered.length,
        unpriced: filtered.filter((entry) => !entry.priced).length,
        has_more: false,
        next_cursor: null,
        capped: false,
      };
      return page;
    },
    detail: async (runId: string, id: number) => {
      mocks.detailKeys.push(`${runId}:${id}`);
      return mocks.details.get(`${runId}:${id}`) ?? null;
    },
    settings: async () => ({ log_bodies: false, ring_capacity: 128, journal_bytes: 0, history_bytes: 0 }),
    setLogBodies: async (on: boolean) => ({ log_bodies: on, ring_capacity: 128, journal_bytes: 0, history_bytes: 0 }),
    clearLive: async () => undefined,
    deleteHistory: async () => ({ deleted_files: 0, bytes: 0 }),
    exportCalls: async () => ({ path: "", count: 0, includes_bodies: false }),
    revealExport: async () => undefined,
  };
}

describe("TraceTab", () => {
  beforeEach(() => {
    mocks.transport = null;
    mocks.rows = [];
    mocks.details = new Map();
    mocks.queries = [];
    mocks.detailKeys = [];
    mocks.failList = 0;
    app.set({ ...fixtureState(), selectedWorkerId: null, workerPanelId: null, traceEntry: null });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("needs the desktop host and says so outside it", () => {
    render(TraceTab);
    expect(screen.getByText("The Trace tab needs the desktop app.")).toBeTruthy();
  });

  it("lists the newest calls with who made them and what they cost", async () => {
    installTransport();
    mocks.rows = [
      row({ id: 9, ts_ms: NOW - 1_000, model: "worker-model", attribution: attribution({ actor_kind: "worker", agent_id: "agent-kai", agent_name: "Kai", worker_id: "kai", plan_id: "c3ba2b58", plan_step: 5 }) }),
      row({ id: 8, ts_ms: NOW - 5_000 }),
    ];
    render(TraceTab);

    const newest = await screen.findByRole("button", { name: /Open call 9:/ });
    expect(newest.textContent).toContain("Kai");
    expect(newest.textContent).toContain("worker-model");
    expect(newest.textContent).toContain("$0.00");
    const oldest = screen.getByRole("button", { name: /Open call 8:/ });
    expect(oldest.textContent).toContain("Lead");
    // The tab asks for the newest 20 and names the scope in plain words.
    expect(mocks.queries[0]).toEqual({ agent: undefined, limit: 20 });
    expect(screen.getByText(/The latest 20 model calls from the lead and every worker/)).toBeTruthy();
  });

  it("filters by the lead and by the open worker", async () => {
    installTransport();
    const worker = fixtureState().workers[0];
    app.set({ ...fixtureState(), selectedWorkerId: worker.id, workerPanelId: null, traceEntry: null });
    mocks.rows = [
      row({ id: 3, attribution: attribution({ actor_kind: "worker", agent_id: `agent-${worker.id}`, agent_name: worker.name, worker_id: worker.id }) }),
      row({ id: 2 }),
    ];
    render(TraceTab);
    await screen.findByRole("button", { name: /Open call 3:/ });

    // The third filter is the worker whose panel is open, by name.
    await fireEvent.click(screen.getByRole("button", { name: "This lead" }));
    await vi.waitFor(() => expect(mocks.queries.at(-1)?.agent).toBe("lead"));
    await fireEvent.click(screen.getByRole("button", { name: worker.name }));
    await vi.waitFor(() => expect(mocks.queries.at(-1)?.agent).toBe(worker.id));
    await fireEvent.click(screen.getByRole("button", { name: "All agents" }));
    await vi.waitFor(() => expect(mocks.queries.at(-1)?.agent).toBeUndefined());
  });

  it("shows one call's Overview, Context and Response in place", async () => {
    installTransport();
    const call = row({ id: 7 });
    mocks.rows = [call];
    mocks.details.set("run-1:7", detail(call));
    render(TraceTab);

    await fireEvent.click(await screen.findByRole("button", { name: /Open call 7:/ }));
    await vi.waitFor(() => expect(mocks.detailKeys).toContain("run-1:7"));
    // The Overview's Timing heading proves the shared detail section rendered.
    expect(await screen.findByRole("heading", { name: /Timing/ })).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Context" }));
    // The Context section shows the system prompt; findAllByText because the
    // message renders once per presentation form (block and raw).
    expect((await screen.findAllByText("You are the lead.")).length).toBeGreaterThan(0);

    await fireEvent.click(screen.getByRole("button", { name: "Response" }));
    expect((await screen.findAllByText("Plan the fix first.")).length).toBeGreaterThan(0);

    await fireEvent.click(screen.getByRole("button", { name: "Back to the call list" }));
    expect(await screen.findByRole("button", { name: /Open call 7:/ })).toBeTruthy();
  });

  it("carries the filter and the open call into the full route", async () => {
    installTransport();
    const worker = fixtureState().workers[0];
    app.set({ ...fixtureState(), selectedWorkerId: worker.id, workerPanelId: null, traceEntry: null });
    const call = row({ id: 5, attribution: attribution({ actor_kind: "worker", agent_id: `agent-${worker.id}`, agent_name: worker.name, worker_id: worker.id }) });
    mocks.rows = [call];
    mocks.details.set("run-1:5", detail(call));
    render(TraceTab);

    await fireEvent.click(await screen.findByRole("button", { name: /Open call 5:/ }));
    await vi.waitFor(() => expect(mocks.detailKeys).toContain("run-1:5"));

    await fireEvent.click(screen.getByRole("button", { name: "Open this call in the full Trace page" }));
    const appState = get(app);
    expect(appState.route).toBe("trace");
    expect(appState.workbenchOpen).toBe(false);
    expect(appState.traceEntry).toEqual({ selected: "run-1:5" });
  });

  it("explains an empty log and points at the route", async () => {
    installTransport();
    render(TraceTab);
    expect(await screen.findByText("No model calls yet.")).toBeTruthy();
    expect(screen.getByText(/appear here the moment they start/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Open the full Trace page" }));
    expect(get(app).route).toBe("trace");
  });

  it("keeps the last good list when a refresh fails, and reports a first-load failure", async () => {
    installTransport();
    mocks.failList = 1;
    render(TraceTab);
    expect(await screen.findByText(/Could not read the model calls: engine offline/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("No model calls yet.")).toBeTruthy();
  });

  it("marks retained metadata and says when the bodies are gone", async () => {
    installTransport();
    const kept = row({ id: 6, retained: true, cost_micros: null, priced: false });
    mocks.rows = [kept];
    // A retained call kept its metadata only: the detail is not in the live log.
    render(TraceTab);

    const call = await screen.findByRole("button", { name: /Open call 6:/ });
    expect(call.textContent).toContain("—");
    await fireEvent.click(call);
    expect(await screen.findByText(/no longer available in the live log/)).toBeTruthy();
  });
});
