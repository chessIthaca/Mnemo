import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import TraceView from "./TraceView.svelte";
import { app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import type {
  TraceAttribution,
  TraceDetail,
  TraceExportRequest,
  TraceListPage,
  TraceListQuery,
  TracePageTransport,
  TraceRow,
} from "../lib/trace";

const mocks = vi.hoisted(() => ({
  transport: null as TracePageTransport | null,
  rows: [] as TraceRow[],
  page: {} as Partial<TraceListPage>,
  details: new Map<string, TraceDetail>(),
  failList: 0,
  queries: [] as TraceListQuery[],
  /** Every export request the page sent, in order. */
  exports: [] as TraceExportRequest[],
  /** Every path the page asked the host to reveal. */
  revealed: [] as string[],
  /** Every detail key the page fetched, in order. */
  detailKeys: [] as string[],
  /** Body-logging switches the page set. */
  bodies: [] as boolean[],
  /** When set, list() filters rows by query.agent the way the backend does. */
  agentFilters: false,
  cleared: 0,
  deletes: 0,
}));

// TraceView builds its own transport on mount; the tests plant one instead,
// keeping every helper in ../lib/trace real so the page is tested end to end.
vi.mock("../lib/trace", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/trace")>();
  return { ...original, createTracePageTransport: () => mocks.transport };
});

function attribution(overrides: Partial<TraceAttribution> = {}): TraceAttribution {
  return {
    actor_kind: "worker",
    agent_id: "design-trace",
    session_id: "session-1",
    agent_name: "design-trace",
    worker_id: "design-trace",
    plan_id: "c3ba2b58",
    plan_step: 0,
    turn_id: "t-1",
    purpose: "turn",
    ...overrides,
  };
}

/** One row, newest-first list order, priced and attributed by default. */
function callRow(overrides: Partial<TraceRow> = {}): TraceRow {
  return {
    id: 1,
    ts_ms: Date.now(),
    model: "kimi-k3.1-pro",
    base_url: "https://api.kimi.invalid/v1",
    provider: "kimi",
    http_status: 200,
    usage: { prompt: 18_240, completion: 1_400, reasoning: 620, cached: 16_800 },
    finish_reason: "tool_calls",
    is_complete: true,
    version: 3,
    ttft_ms: 900,
    generation_ms: 4_000,
    reasoning_ms: 1_200,
    connect_ms: 300,
    upload_ms: null,
    server_ms: null,
    tools_ms: 8_000,
    prep_ms: 500,
    compact_ms: null,
    backoff_ms: null,
    stall_ms: null,
    chunks: null,
    error: null,
    cancelled: false,
    response_truncated: false,
    response_evicted: false,
    guard_cut: null,
    run_id: "run-1",
    attribution: attribution(),
    cost_micros: 81_000,
    priced: true,
    retained: false,
    ...overrides,
  };
}

/** One full record: the row plus an SSE body, two messages and a tool call. */
function callDetail(row: TraceRow, overrides: Partial<TraceDetail> = {}): TraceDetail {
  return {
    ...row,
    request_json: {
      messages: [
        { role: "system", content: "You are the lead." },
        { role: "user", content: "Fix the failing test." },
      ],
    },
    response_raw: [
      'data: {"choices":[{"delta":{"reasoning_content":"checking "}}]}',
      'data: {"choices":[{"delta":{"content":"The answer is 42."}}]}',
      "data: [DONE]",
    ].join("\n"),
    request_truncated: false,
    raw_tool_calls: [
      { id: "tc-1", name: "read_file", arguments: '{"path":"src/lib.rs"}', truncated: false },
    ],
    ...overrides,
  };
}

function detailKey(row: TraceRow): string {
  return `${row.run_id}:${row.id}`;
}

/**
 * The backend's agent_matches contract (trace_view.rs): "lead" selects the
 * lead's rows by actor kind, "unattributed" the legacy rows without
 * attribution, any other value that worker/agent/session id — and a null
 * attribution never matches "lead".
 */
function rowsMatchingAgent(rows: TraceRow[], agent: string | undefined): TraceRow[] {
  if (agent === undefined || !mocks.agentFilters) return rows;
  return rows.filter((row) => {
    const attribution = row.attribution;
    if (attribution === null) return agent === "unattributed";
    if (agent === "lead") return attribution.actor_kind === "lead";
    if (agent === "unattributed") return false;
    return attribution.worker_id === agent || attribution.agent_id === agent || attribution.session_id === agent;
  });
}

/** Plant the mock IPC surface the page talks to. */
function installTransport(): void {
  mocks.transport = {
    list: async (query: TraceListQuery) => {
      mocks.queries.push(query);
      if (mocks.failList > 0) {
        mocks.failList -= 1;
        throw new Error("engine offline");
      }
      const rows = rowsMatchingAgent(mocks.rows, query.agent);
      return {
        rows,
        total: rows.length,
        unpriced: 0,
        has_more: false,
        next_cursor: null,
        capped: false,
        ...mocks.page,
      };
    },
    detail: async (runId: string, id: number) => {
      mocks.detailKeys.push(`${runId}:${id}`);
      return mocks.details.get(`${runId}:${id}`) ?? null;
    },
    settings: async () => ({
      log_bodies: mocks.bodies.length > 0 ? mocks.bodies[mocks.bodies.length - 1] : false,
      ring_capacity: 128,
      journal_bytes: 4096,
      history_bytes: 51_200,
    }),
    setLogBodies: async (on: boolean) => {
      mocks.bodies.push(on);
      return { log_bodies: on, ring_capacity: 128, journal_bytes: 4096, history_bytes: 51_200 };
    },
    clearLive: async () => {
      mocks.cleared += 1;
    },
    deleteHistory: async () => {
      mocks.deletes += 1;
      return { deleted_files: 2, bytes: 51_200 };
    },
    exportCalls: async (request: TraceExportRequest) => {
      mocks.exports.push(request);
      return { path: "/project/.coding/logs/trace-exports/trace-20261102-140000.jsonl", count: request.keys?.length ?? mocks.rows.length, includes_bodies: false };
    },
    revealExport: async (path: string) => {
      mocks.revealed.push(path);
    },
  };
}

async function flush(ms: number): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
}

describe("TraceView", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    window.localStorage.clear();
    mocks.transport = null;
    mocks.rows = [];
    mocks.page = {};
    mocks.details = new Map();
    mocks.failList = 0;
    mocks.queries = [];
    mocks.exports = [];
    mocks.detailKeys = [];
    mocks.revealed = [];
    mocks.bodies = [];
    mocks.agentFilters = false;
    mocks.cleared = 0;
    mocks.deletes = 0;
    app.set({ ...fixtureState(), route: "lead", traceEntry: null });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("explains itself outside the desktop app instead of an empty page", async () => {
    render(TraceView);
    expect(screen.getByText("Model-call tracing runs in the desktop app")).toBeTruthy();
  });

  it("lists every call with its actor, status, tokens and cost at a glance", async () => {
    installTransport();
    const leadCall = callRow({ id: 1, model: "lead-model", attribution: attribution({ actor_kind: "lead", agent_id: "lead", worker_id: null, plan_id: null, plan_step: null, turn_id: null }) });
    const workerCall = callRow({ id: 2, ts_ms: Date.now() - 60_000, attribution: attribution({ turn_id: "t-9" }) });
    const unpriced = callRow({
      id: 3,
      ts_ms: Date.now() - 90_000,
      model: "mystery-model",
      cost_micros: null,
      priced: false,
      attribution: attribution({ actor_kind: "subagent", agent_id: "helper", agent_name: "helper", worker_id: null, plan_id: null, plan_step: null, turn_id: null }),
    });
    mocks.rows = [leadCall, workerCall, unpriced];
    render(TraceView);

    await vi.waitFor(() => expect(screen.getByRole("button", { name: /mystery-model/ })).toBeTruthy());
    expect(screen.getByRole("button", { name: /lead-model/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /kimi-k3.1-pro/ })).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /Complete/ })).toHaveLength(3);
    expect(screen.getByRole("button", { name: /design-trace/ })).toBeTruthy();
    // 3 calls with tokens and a priced total, plus the unpriced marker.
    expect(screen.getByText(/3 calls · \S+ tokens ·/)).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /92% cached/ })).toHaveLength(3);
  });

  it("sends the agent filter to the backend and restores it as a chip", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /kimi-k3.1-pro/ })).toBeTruthy());

    const agentSelect = screen.getByRole("combobox", { name: "Agent" }) as HTMLSelectElement;
    await fireEvent.change(agentSelect, { target: { value: "design-trace" } });

    await vi.waitFor(() => {
      expect(mocks.queries.some((query) => query.agent === "design-trace")).toBe(true);
    });
    expect(screen.getByRole("button", { name: "Remove filter Agent: design-trace" })).toBeTruthy();
  });

  it("shows removable chips and a no-match state when a filter empties the list", async () => {
    installTransport();
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByText(/0 calls ·/)).toBeTruthy());

    const agentSelect = screen.getByRole("combobox", { name: "Agent" }) as HTMLSelectElement;
    await fireEvent.change(agentSelect, { target: { value: "unattributed" } });

    await vi.waitFor(() => {
      expect(mocks.queries.some((query) => query.agent === "unattributed")).toBe(true);
    });
    await vi.waitFor(() => expect(screen.getByText("No calls match these filters.")).toBeTruthy());
    expect(screen.getByRole("button", { name: "Remove filter Agent: unattributed" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    await vi.waitFor(() => {
      // The reset restores the Lead default, not the unscoped "all".
      expect(mocks.queries[mocks.queries.length - 1].agent).toBe("lead");
    });
    expect(screen.queryByText("No calls match these filters.")).toBeNull();
  });

  it("opens on the Lead with the charts at the top scoped to the lead's calls", async () => {
    installTransport();
    // The mock filters by query.agent exactly as the backend's agent_matches
    // does, so the walk exercises the real scoping contract end to end.
    mocks.agentFilters = true;
    const leadCall = callRow({
      id: 1,
      model: "lead-model",
      attribution: attribution({ actor_kind: "lead", agent_id: "lead", worker_id: null, plan_id: null, plan_step: null, turn_id: null }),
    });
    const workerCall = callRow({ id: 2, ts_ms: Date.now() - 60_000, attribution: attribution({ turn_id: "t-9" }) });
    const legacy = callRow({ id: 3, ts_ms: Date.now() - 90_000, model: "legacy-model", attribution: null });
    mocks.rows = [leadCall, workerCall, legacy];
    render(TraceView);

    // The tab opens with Lead selected and asks the backend for the lead's calls.
    const agentSelect = screen.getByRole("combobox", { name: "Agent" }) as HTMLSelectElement;
    await vi.waitFor(() => expect(agentSelect.value).toBe("lead"));
    await vi.waitFor(() => expect(mocks.queries.some((query) => query.agent === "lead")).toBe(true));

    // The Insights charts sit open at the top, over only the lead's calls —
    // the worker's and the unattributed legacy row never match "lead".
    expect(screen.getByRole("region", { name: "Insights" })).toBeTruthy();
    await vi.waitFor(() => expect(screen.getByText(/Charts for 1 filtered call · lead · this session/)).toBeTruthy());
    expect(screen.getByRole("button", { name: /lead-model/ })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /kimi-k3\.1-pro/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /legacy-model/ })).toBeNull();

    // A worker is selectable from the same dropdown and scopes list and charts.
    await fireEvent.change(agentSelect, { target: { value: "design-trace" } });
    // Wait on the worker's row: the scope label flips before the reload lands.
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /kimi-k3\.1-pro/ })).toBeTruthy());
    expect(screen.getByText(/Charts for 1 filtered call · design-trace · this session/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /lead-model/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Remove filter Agent: design-trace" })).toBeTruthy();

    // Removing the agent chip returns to the Lead default.
    await fireEvent.click(screen.getByRole("button", { name: "Remove filter Agent: design-trace" }));
    await vi.waitFor(() => expect(agentSelect.value).toBe("lead"));
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /lead-model/ })).toBeTruthy());
    expect(screen.getByText(/Charts for 1 filtered call · lead · this session/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Remove filter Agent: design-trace" })).toBeNull();

    // All remains available and shows every actor.
    await fireEvent.change(agentSelect, { target: { value: "all" } });
    await vi.waitFor(() => expect(screen.getByText(/Charts for 3 filtered calls · this session/)).toBeTruthy());
    expect(screen.getByRole("button", { name: /lead-model/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /kimi-k3\.1-pro/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /legacy-model/ })).toBeTruthy();
  });

  it("debounces the search box and carries its scope", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /kimi-k3.1-pro/ })).toBeTruthy());

    const search = screen.getByRole("searchbox", { name: "Search calls" }) as HTMLInputElement;
    await fireEvent.input(search, { target: { value: "read_file" } });
    expect(mocks.queries.some((query) => query.search === "read_file")).toBe(false);

    await flush(300);
    await vi.waitFor(() => {
      expect(mocks.queries.some((query) => query.search === "read_file" && query.search_scope === "all")).toBe(true);
    });

    const scope = screen.getByRole("combobox", { name: "Search scope" }) as HTMLSelectElement;
    await fireEvent.change(scope, { target: { value: "response" } });
    await flush(0);
    await vi.waitFor(() => {
      expect(mocks.queries.some((query) => query.search === "read_file" && query.search_scope === "response")).toBe(true);
    });
  });

  it("groups by day by default and by actor when the Group control says Agent", async () => {
    installTransport();
    const leadCall = callRow({ id: 1, model: "lead-model", attribution: attribution({ actor_kind: "lead", agent_id: "lead", worker_id: null, plan_id: null, plan_step: null, turn_id: null }) });
    mocks.rows = [leadCall, callRow({ id: 2, ts_ms: Date.now() - 60_000, attribution: attribution({ turn_id: "t-9" }) })];
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("heading", { name: "Today" })).toBeTruthy());

    const group = screen.getByRole("combobox", { name: "Group" }) as HTMLSelectElement;
    await fireEvent.change(group, { target: { value: "agent" } });

    await vi.waitFor(() => expect(screen.getByRole("heading", { name: "Lead" })).toBeTruthy());
    expect(screen.getByRole("heading", { name: "design-trace" })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Today" })).toBeNull();
  });

  it("counts new calls behind the follow pill instead of moving the list", async () => {
    installTransport();
    const head = callRow({ id: 1, model: "head-model", ts_ms: Date.now() - 10_000 });
    const older = callRow({ id: 2, model: "older-model", ts_ms: Date.now() - 60_000, attribution: attribution({ turn_id: "t-9" }) });
    const newer = callRow({ id: 3, model: "newer-model", ts_ms: Date.now() });
    mocks.rows = [head, older];
    mocks.details.set(detailKey(older), callDetail(older));
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /older-model/ })).toBeTruthy());

    // Selecting an older call stops following.
    await fireEvent.click(screen.getByRole("button", { name: /older-model/ }));
    await vi.waitFor(() => expect(screen.getByRole("button", { name: "Overview" })).toBeTruthy());
    const followToggle = screen.getByRole("checkbox", { name: "Follow new calls" }) as HTMLInputElement;
    expect(followToggle.checked).toBe(false);

    // A new call arrives: the pill counts it, the list stays stable.
    mocks.rows = [newer, head, older];
    await flush(1_500);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: "1 new call — Follow" })).toBeTruthy());
    expect(screen.queryByRole("button", { name: /newer-model/ })).toBeNull();
    // The selection survived.
    expect(screen.getByRole("button", { name: "Overview" })).toBeTruthy();

    // Following again reveals the new call at the head.
    await fireEvent.click(screen.getByRole("button", { name: "1 new call — Follow" }));
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /newer-model/ })).toBeTruthy());
    expect(screen.queryByRole("button", { name: "1 new call — Follow" })).toBeNull();
  });

  it("updates an in-flight row in place from the poll, never duplicating it", async () => {
    installTransport();
    const live = callRow({ id: 1, model: "live-model", is_complete: false, ttft_ms: null, connect_ms: null, finish_reason: null, http_status: null, usage: null, version: 1 });
    mocks.rows = [live];
    render(TraceView);
    // The POST is out, the body is still being written: the finer live
    // status shows Uploading where the old model said only Waiting.
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /Uploading/ })).toBeTruthy());

    // The upload finishes: the same row now waits on the server.
    const waiting = callRow({ id: 1, model: "live-model", is_complete: false, ttft_ms: null, connect_ms: null, upload_ms: 120, version: 2 });
    mocks.rows = [waiting];
    await flush(1_500);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /Waiting for first token/ })).toBeTruthy());

    const finished = callRow({ id: 1, model: "live-model", is_complete: true, version: 3 });
    mocks.rows = [finished];
    await flush(1_500);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /live-model/ })).toBeTruthy());
    const rowButtons = screen.getAllByRole("button", { name: /live-model/ });
    expect(rowButtons).toHaveLength(1);
    expect(screen.getByRole("button", { name: /Complete/ })).toBeTruthy();
  });

  it("opens the selected call: Overview, Context with prefix reuse, Response, Tools and Raw", async () => {
    installTransport();
    // Same agent, purpose and model, one turn apart: the older call is the
    // comparable predecessor the Context tab measures against.
    const earlier = callRow({ id: 2, ts_ms: Date.now() - 60_000, attribution: attribution({ turn_id: "t-8" }) });
    const selected = callRow({ id: 1, ts_ms: Date.now() - 10_000, attribution: attribution({ turn_id: "t-9" }) });
    mocks.rows = [selected, earlier];
    mocks.details.set(detailKey(selected), callDetail(selected));
    // The previous comparable call feeds the Context tab's prefix comparison.
    mocks.details.set(detailKey(earlier), callDetail(earlier, {
      request_json: {
        messages: [
          { role: "system", content: "You are the lead." },
          { role: "user", content: "Review the failing test." },
        ],
      },
    }));
    render(TraceView);
    await vi.waitFor(() => expect(screen.getAllByRole("button", { name: /kimi-k3\.1-pro/ })).toHaveLength(2));

    // The list is newest first, so the first row is the call under inspection.
    await fireEvent.click(screen.getAllByRole("button", { name: /kimi-k3\.1-pro/ })[0]);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: "Overview" })).toBeTruthy());

    // Overview is the default tab, with the glossary a keypress away.
    expect(screen.getByText("Who")).toBeTruthy();
    expect(screen.getByRole("button", { name: "What does “Estimated cost” mean?" })).toBeTruthy();

    // Context compares with the previous call and reads as a message wall.
    await fireEvent.click(screen.getByRole("button", { name: "Context (2)" }));
    await vi.waitFor(() => expect(screen.getByText(/shares this beginning/)).toBeTruthy());
    expect(screen.getByText("System")).toBeTruthy();
    expect(screen.getByText("User")).toBeTruthy();

    // Response splits reasoning from the answer.
    await fireEvent.click(screen.getByRole("button", { name: "Response" }));
    await vi.waitFor(() => expect(screen.getByText("The answer is 42.")).toBeTruthy());
    expect(screen.getByRole("heading", { name: "Reasoning" })).toBeTruthy();

    // Tools shows the delivered call with parsed JSON.
    await fireEvent.click(screen.getByRole("button", { name: "Tools (1)" }));
    await vi.waitFor(() => expect(screen.getByText("read_file")).toBeTruthy());

    // Raw keeps the exact payloads.
    await fireEvent.click(screen.getByRole("button", { name: "Raw" }));
    await vi.waitFor(() => expect(screen.getByText(/"Fix the failing test\."/)).toBeTruthy());
  });

  it("keeps the last data and offers a retry when the list load fails", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    // One failure for the silent options probe, one for the page's own load.
    mocks.failList = 2;
    render(TraceView);

    await vi.waitFor(() => expect(screen.getByRole("alert")).toBeTruthy());
    expect(screen.getByText(/Could not read the model calls: engine offline/)).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await vi.waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(screen.getByRole("button", { name: /kimi-k3.1-pro/ })).toBeTruthy();
  });

  it("welcomes an empty log with the Open Lead action", async () => {
    installTransport();
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByText("Model calls will appear here when the lead or a worker runs.")).toBeTruthy());
    expect(screen.getByRole("button", { name: "Open Lead" })).toBeTruthy();
  });

  it("consumes an entry hand-over with its filters and clears it afterwards", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    app.set({ ...fixtureState(), route: "lead", traceEntry: { agent: "design-trace" } });
    render(TraceView);

    await vi.waitFor(() => {
      expect((screen.getByRole("combobox", { name: "Agent" }) as HTMLSelectElement).value).toBe("design-trace");
    });
    await vi.waitFor(() => expect(mocks.queries.some((query) => query.agent === "design-trace")).toBe(true));
    expect(get(app).traceEntry).toBeNull();
    expect(screen.getByRole("button", { name: "Remove filter Agent: design-trace" })).toBeTruthy();
  });

  it("restores the saved filters and grouping per project", async () => {
    installTransport();
    const leadCall = callRow({ id: 1, model: "lead-model", attribution: attribution({ actor_kind: "lead", agent_id: "lead", worker_id: null, plan_id: null, plan_step: null, turn_id: null }) });
    const workerCall = callRow({ id: 2, ts_ms: Date.now() - 60_000, attribution: attribution({ turn_id: "t-9" }) });
    mocks.rows = [leadCall, workerCall];
    window.localStorage.setItem(
      "mnemo2.trace.mnemo",
      JSON.stringify({ grouping: "turn", agent: "design-trace", insightsOpen: true }),
    );
    render(TraceView);

    await vi.waitFor(() =>
      expect((screen.getByRole("combobox", { name: "Group" }) as HTMLSelectElement).value).toBe("turn"),
    );
    // The agent option arrives with the async probe; the select value follows it.
    await vi.waitFor(() =>
      expect((screen.getByRole("combobox", { name: "Agent" }) as HTMLSelectElement).value).toBe("design-trace"),
    );
    await vi.waitFor(() => expect(screen.getByRole("region", { name: "Insights" })).toBeTruthy());
    expect(screen.getByRole("heading", { name: /Turn t-9 — design-trace/ })).toBeTruthy();
  });

  it("notes when the backend capped the result set", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    mocks.page = { capped: true };
    render(TraceView);
    await vi.waitFor(() =>
      expect(screen.getByText("Showing first 1,000 matches; narrow the time range.")).toBeTruthy(),
    );
  });

  it("pages older calls through Load more and the cursor", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    mocks.page = { has_more: true, next_cursor: "cursor-2" };
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: "Load more" })).toBeTruthy());

    await fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    await vi.waitFor(() => expect(mocks.queries.some((query) => query.cursor === "cursor-2")).toBe(true));
  });

  it("mounts the Insights charts open over the loaded rows with the filter scope", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /kimi-k3.1-pro/ })).toBeTruthy());

    // The charts open with the tab, scoped to the Lead's calls.
    await vi.waitFor(() => expect(screen.getByRole("region", { name: "Insights" })).toBeTruthy());
    expect(screen.getByText(/Charts for 1 filtered call · lead · this session/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Hide charts" }));
    await vi.waitFor(() => expect(screen.queryByRole("region", { name: "Insights" })).toBeNull());
  });

  it("keeps the Insights charts closed when the saved preference says so", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    window.localStorage.setItem("mnemo2.trace.mnemo", JSON.stringify({ insightsOpen: false }));
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /kimi-k3.1-pro/ })).toBeTruthy());
    expect(screen.queryByRole("region", { name: "Insights" })).toBeNull();
    expect(screen.getByRole("button", { name: "Insights" })).toBeTruthy();
  });

  it("the insights divider is a horizontal separator under the charts", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    const { container } = render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("region", { name: "Insights" })).toBeTruthy());

    const divider = screen.getByRole("button", { name: "Resize the charts (drag or arrow keys)" });
    expect(divider.getAttribute("aria-label")).toBe("Resize the charts (drag or arrow keys)");
    // The bar sits directly under the charts body.
    expect(container.querySelector(".insights-body")?.nextElementSibling).toBe(divider);

    await fireEvent.click(screen.getByRole("button", { name: "Hide charts" }));
    await vi.waitFor(() =>
      expect(screen.queryByRole("button", { name: "Resize the charts (drag or arrow keys)" })).toBeNull(),
    );
  });

  it("dragging the insights divider changes the charts height within its bounds", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    const { container } = render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("region", { name: "Insights" })).toBeTruthy());

    const divider = screen.getByRole("button", { name: "Resize the charts (drag or arrow keys)" });
    const body = container.querySelector<HTMLElement>(".insights-body");
    expect(body?.style.height).toBe("");

    const rect = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      x: 0,
      y: 100,
      top: 100,
      left: 0,
      right: 800,
      bottom: 600,
      width: 800,
      height: 500,
      toJSON: () => ({}),
    } as DOMRect);

    await fireEvent.pointerDown(divider, { pointerId: 1 });
    await fireEvent.pointerMove(divider, { pointerId: 1, clientY: 350 });
    expect(body?.style.height).toBe("250px");

    // Dragging far past the view clamps to the room that leaves the call list visible.
    await fireEvent.pointerMove(divider, { pointerId: 1, clientY: 5000 });
    const capped = Number((body?.style.height ?? "").replace("px", ""));
    expect(capped).toBeLessThanOrEqual(900);
    expect(capped).toBeGreaterThan(160);

    await fireEvent.pointerMove(divider, { pointerId: 1, clientY: 40 });
    expect(body?.style.height).toBe("160px");

    await fireEvent.pointerUp(divider, { pointerId: 1 });
    expect(JSON.parse(window.localStorage.getItem("mnemo2.trace.mnemo") ?? "{}").insightsHeight).toBe(160);

    rect.mockRestore();
  });

  it("arrow keys shift and Home resize and reset the insights height", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    window.localStorage.setItem("mnemo2.trace.mnemo", JSON.stringify({ insightsOpen: true, insightsHeight: 400 }));
    const { container } = render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("region", { name: "Insights" })).toBeTruthy());

    const divider = screen.getByRole("button", { name: "Resize the charts (drag or arrow keys)" });
    const body = container.querySelector<HTMLElement>(".insights-body");
    await vi.waitFor(() => expect(body?.style.height).toBe("400px"));

    await fireEvent.keyDown(divider, { key: "ArrowDown" });
    expect(body?.style.height).toBe("420px");
    await fireEvent.keyDown(divider, { key: "ArrowDown", shiftKey: true });
    expect(body?.style.height).toBe("480px");
    await fireEvent.keyDown(divider, { key: "ArrowUp" });
    expect(body?.style.height).toBe("460px");

    await fireEvent.keyDown(divider, { key: "Home" });
    expect(body?.style.height).toBe("");
    expect(JSON.parse(window.localStorage.getItem("mnemo2.trace.mnemo") ?? "{}").insightsHeight).toBeNull();
  });

  it("the saved insights height is restored, clamped and ignored when not a number", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];

    window.localStorage.setItem("mnemo2.trace.mnemo", JSON.stringify({ insightsOpen: true, insightsHeight: 400 }));
    const restored = render(TraceView);
    await vi.waitFor(() =>
      expect(restored.container.querySelector<HTMLElement>(".insights-body")?.style.height).toBe("400px"),
    );
    restored.unmount();

    window.localStorage.setItem("mnemo2.trace.mnemo", JSON.stringify({ insightsOpen: true, insightsHeight: 99999 }));
    const clamped = render(TraceView);
    await vi.waitFor(() =>
      expect(clamped.container.querySelector<HTMLElement>(".insights-body")?.style.height).toBe("900px"),
    );
    clamped.unmount();

    window.localStorage.setItem("mnemo2.trace.mnemo", JSON.stringify({ insightsOpen: true, insightsHeight: "tall" }));
    const textual = render(TraceView);
    await vi.waitFor(() => expect(textual.container.querySelector(".insights-body")).toBeTruthy());
    expect(textual.container.querySelector<HTMLElement>(".insights-body")?.style.height).toBe("");
    textual.unmount();

    window.localStorage.setItem("mnemo2.trace.mnemo", JSON.stringify({ insightsOpen: true, insightsHeight: null }));
    const absent = render(TraceView);
    await vi.waitFor(() => expect(absent.container.querySelector(".insights-body")).toBeTruthy());
    expect(absent.container.querySelector<HTMLElement>(".insights-body")?.style.height).toBe("");
  });

  it("exports the open call, shows the bundle path and reveals it on demand", async () => {
    installTransport();
    const row = callRow({ id: 7 });
    mocks.rows = [row];
    mocks.details.set(detailKey(row), callDetail(row));
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /kimi-k3.1-pro/ })).toBeTruthy());

    // No call open yet: a single-call export has nothing to write.
    expect((screen.getByRole("button", { name: "Current call" }) as HTMLButtonElement).disabled).toBe(true);

    await fireEvent.click(screen.getByRole("button", { name: /kimi-k3.1-pro/ }));
    await vi.waitFor(() => expect(mocks.detailKeys).toContain("run-1:7"));

    await fireEvent.click(screen.getByRole("button", { name: "Current call" }));
    await vi.waitFor(() => expect(mocks.exports).toEqual([{ scope: "call", keys: ["run-1:7"] }]));

    await vi.waitFor(() => expect(screen.getByText(/Exported 1 call to/)).toBeTruthy());
    const path = "/project/.coding/logs/trace-exports/trace-20261102-140000.jsonl";
    expect(screen.getByText(path)).toBeTruthy();
    // The bundle carries metadata only, and the panel says so.
    expect(screen.getByText(/metadata only, no prompt or response text/)).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Reveal" }));
    await vi.waitFor(() => expect(mocks.revealed).toEqual([path]));
  });

  it("exports the ticked calls by their durable keys", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 11 }), callRow({ id: 12, ts_ms: Date.now() - 1_000 })];
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("checkbox", { name: "Select call 11 for export" })).toBeTruthy());

    expect((screen.getByRole("button", { name: "Selected calls (0)" }) as HTMLButtonElement).disabled).toBe(true);

    await fireEvent.click(screen.getByRole("checkbox", { name: "Select call 11 for export" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "Select call 12 for export" }));
    await vi.waitFor(() => expect(screen.getByRole("button", { name: "Selected calls (2)" })).toBeTruthy());

    await fireEvent.click(screen.getByRole("button", { name: "Selected calls (2)" }));
    await vi.waitFor(() => expect(mocks.exports).toHaveLength(1));
    expect(mocks.exports[0].scope).toBe("selected");
    expect([...(mocks.exports[0].keys ?? [])].sort()).toEqual(["run-1:11", "run-1:12"]);
  });

  it("exports every call the current filters match", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 21 }), callRow({ id: 22, ts_ms: Date.now() - 2_000 })];
    render(TraceView);
    await vi.waitFor(() => expect(screen.getByRole("button", { name: /All filtered \(2\)/ })).toBeTruthy());

    await fireEvent.click(screen.getByRole("button", { name: /All filtered \(2\)/ }));
    await vi.waitFor(() => expect(mocks.exports).toHaveLength(1));
    expect(mocks.exports[0].scope).toBe("filtered");
    expect(mocks.exports[0].query?.limit).toBeGreaterThan(0);
  });

  it("turns on writing full bodies to disk from the sensitive switch", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 31 })];
    render(TraceView);
    const name = /Save full prompts and responses to disk/;
    await vi.waitFor(() => expect(screen.getByRole("checkbox", { name })).toBeTruthy());
    // The switch stays disabled until the settings arrive; the note proves they did.
    await vi.waitFor(() => expect(screen.getByText(/128 calls kept in memory/)).toBeTruthy());
    expect((screen.getByRole("checkbox", { name }) as HTMLInputElement).checked).toBe(false);

    await fireEvent.click(screen.getByRole("checkbox", { name }));
    await vi.waitFor(() => expect(mocks.bodies).toEqual([true]));
    await vi.waitFor(() => expect((screen.getByRole("checkbox", { name }) as HTMLInputElement).checked).toBe(true));
  });

  it("confirms before clearing the live calls and names what is dropped", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 41 })];
    render(TraceView);
    // The confirm card quotes the ring size, so wait for the settings first.
    await vi.waitFor(() => expect(screen.getByText(/128 calls kept in memory/)).toBeTruthy());

    await fireEvent.click(screen.getByRole("button", { name: /Clear live calls/ }));
    await vi.waitFor(() =>
      expect(screen.getByRole("alertdialog", { name: "Confirm clearing the live calls" })).toBeTruthy(),
    );
    const card = screen.getByRole("alertdialog", { name: "Confirm clearing the live calls" });
    expect(card.textContent).toContain("drops the 128 calls held in memory");
    expect(card.textContent).toContain("Retained history is not touched");
    // Nothing is dropped before the confirmation is accepted.
    expect(mocks.cleared).toBe(0);

    await fireEvent.click(screen.getByRole("button", { name: "Yes, clear live calls" }));
    await vi.waitFor(() => expect(mocks.cleared).toBe(1));
    await vi.waitFor(() =>
      expect(screen.queryByRole("alertdialog", { name: "Confirm clearing the live calls" })).toBeNull(),
    );
  });

  it("confirms the retained-history delete with its size and reports the outcome", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 51, retained: true })];
    render(TraceView);
    // The confirm card quotes the retained size, so wait for the settings first.
    await vi.waitFor(() => expect(screen.getByText(/128 calls kept in memory/)).toBeTruthy());

    await fireEvent.click(screen.getByRole("button", { name: /Delete retained history/ }));
    await vi.waitFor(() =>
      expect(screen.getByRole("alertdialog", { name: "Confirm deleting retained history" })).toBeTruthy(),
    );
    const card = screen.getByRole("alertdialog", { name: "Confirm deleting retained history" });
    expect(card.textContent).toContain("50.0 KB");
    expect(card.textContent).toContain("1 retained call in this view");
    expect(mocks.deletes).toBe(0);

    await fireEvent.click(screen.getByRole("button", { name: "Yes, delete retained history" }));
    await vi.waitFor(() => expect(mocks.deletes).toBe(1));
    await vi.waitFor(() => expect(screen.getByText(/Deleted 2 retained files \(50\.0 KB\)/)).toBeTruthy());
  });

  it("opens the call an entry point handed over by its durable key", async () => {
    installTransport();
    const row = callRow({ id: 61, run_id: "run-9" });
    mocks.details.set("run-9:61", callDetail(row));
    app.set({ ...fixtureState(), route: "lead", traceEntry: { agent: "design-trace", selected: "run-9:61" } });
    render(TraceView);

    await vi.waitFor(() => expect(mocks.detailKeys).toContain("run-9:61"));
    // The hand-over is one-shot: the tab consumes it.
    expect(get(app).traceEntry).toBeNull();
  });

  it("shows one column in a narrow panel and two in a wide one", async () => {
    installTransport();
    mocks.rows = [callRow({ id: 1 })];
    const callbacks: ResizeObserverCallback[] = [];
    class StubResizeObserver {
      constructor(callback: ResizeObserverCallback) {
        callbacks.push(callback);
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    const originalObserver = globalThis.ResizeObserver;
    globalThis.ResizeObserver = StubResizeObserver as unknown as typeof ResizeObserver;
    let width = 600;
    const widthGetter = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => width });
    try {
      const { container } = render(TraceView);
      await vi.waitFor(() => expect(callbacks.length).toBeGreaterThan(0));
      expect(container.querySelector(".trace-columns")).toBeNull();

      width = 1200;
      for (const callback of callbacks) callback([], {} as ResizeObserver);
      await vi.waitFor(() => expect(container.querySelector(".trace-columns")).toBeTruthy());
    } finally {
      if (originalObserver) globalThis.ResizeObserver = originalObserver;
      else delete (globalThis as { ResizeObserver?: typeof ResizeObserver }).ResizeObserver;
      if (widthGetter) Object.defineProperty(HTMLElement.prototype, "clientWidth", widthGetter);
      else delete (HTMLElement.prototype as { clientWidth?: number }).clientWidth;
    }
  });
});
