import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createGraphTransport,
  type GraphContext,
  type GraphImpactSymbol,
  type GraphNeighbourhood,
  type GraphNode,
  type GraphOverviewNode,
  type GraphStatus,
  type GraphSymbol,
  type GraphTransport,
} from "../lib/graph";
import { app } from "../lib/store";
import GraphView from "./GraphView.svelte";

const mocks = vi.hoisted(() => ({ transport: null as unknown }));

vi.mock("../lib/graph", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/graph")>();
  return { ...original, createGraphTransport: () => mocks.transport };
});

const ENGINE: GraphNode = {
  id: "src/headless/engine.rs::Engine::stats::680",
  name: "Engine::stats",
  kind: "method",
  file: "src/headless/engine.rs",
  line: 680,
  degree: 42,
};
const RUN_TURN: GraphSymbol = {
  id: "src/agent/turn.rs::run_turn::812",
  name: "run_turn",
  kind: "function",
  file: "src/agent/turn.rs",
  line: 812,
};
const RUN_TURN_NODE: GraphNode = { ...RUN_TURN, degree: 9 };
const SAVE: GraphSymbol = {
  id: "src/memory/mod.rs::save::30",
  name: "save",
  kind: "method",
  file: "src/memory/mod.rs",
  line: 30,
};
const GENERATE: GraphSymbol = {
  id: "src/report.rs::generate::90",
  name: "generate",
  kind: "function",
  file: "src/report.rs",
  line: 90,
};

function status(overrides: Partial<GraphStatus> = {}): GraphStatus {
  const now = Math.floor(Date.now() / 1000) - 7 * 60;
  return { available: true, indexing: false, files: 3945, symbols: 12840, edges: 34210, last_indexed_at: now, ...overrides };
}

/** Seed the host's indexing snapshot: one code pass at `done`/`total` files. */
function seedCodePass(state: "running" | "done", done = 1200, total = 2400) {
  app.update((current) => ({
    ...current,
    indexing: {
      revision: (current.indexing?.revision ?? 0) + 1,
      firstRun: false,
      code: { state, done, total, detail: null },
      memory: null,
    },
  }));
}

function makeTransport(overrides: Partial<GraphTransport> = {}): GraphTransport {
  const neighbourhoods: Record<string, GraphNeighbourhood> = {
    [ENGINE.id]: {
      center: ENGINE,
      nodes: [RUN_TURN_NODE, { ...SAVE, degree: 6 }],
      edges: [
        { from: RUN_TURN.id, to: ENGINE.id, kind: "calls" },
        { from: ENGINE.id, to: SAVE.id, kind: "calls" },
      ],
      truncated: false,
    },
    [RUN_TURN.id]: {
      center: RUN_TURN_NODE,
      nodes: [ENGINE, { ...SAVE, degree: 6 }],
      edges: [{ from: RUN_TURN.id, to: ENGINE.id, kind: "calls" }],
      truncated: false,
    },
  };
  const contexts: Record<string, GraphContext> = {
    [ENGINE.id]: {
      symbol: ENGINE,
      callers: [RUN_TURN],
      callees: [SAVE, GENERATE],
      caller_total: 1,
      callee_total: 12,
    },
    [RUN_TURN.id]: { symbol: RUN_TURN_NODE, callers: [], callees: [ENGINE], caller_total: 0, callee_total: 1 },
  };
  const impacts: Record<string, GraphImpactSymbol[]> = {
    [ENGINE.id]: [
      { ...RUN_TURN, distance: 1 },
      { ...SAVE, distance: 1 },
      { ...GENERATE, distance: 2 },
      { ...GENERATE, id: "src/report.rs::sweep::91", name: "sweep", distance: 3 },
    ],
    [RUN_TURN.id]: [{ ...ENGINE, distance: 1 }],
  };
  const base: GraphTransport = {
    status: vi.fn(async () => status()),
    refresh: vi.fn(async () => status({ indexing: true })),
    search: vi.fn(async (query: string, kind: string | null) => {
      const all = [ENGINE, RUN_TURN, SAVE, GENERATE].filter((entry) => entry.name.toLowerCase().includes(query.toLowerCase()));
      return kind ? all.filter((entry) => entry.kind === kind) : all;
    }),
    neighbourhood: vi.fn(async (symbolId: string, depth = 1) => neighbourhoods[symbolId] ?? neighbourhoods[ENGINE.id] ?? null as never),
    impact: vi.fn(async (symbolId: string) => impacts[symbolId] ?? []),
    top: vi.fn(async () => [ENGINE, RUN_TURN_NODE, { ...SAVE, degree: 6 }]),
    overview: vi.fn(async (level = 2, limit = 60) => {
      const modules: GraphOverviewNode[] = [
        {
          id: "src/agent",
          label: "agent",
          kind: null,
          file: "src/agent",
          line: 0,
          symbols: 12,
          degree: 4,
          top: [RUN_TURN_NODE, { ...SAVE, degree: 6 }],
        },
        {
          id: "src/memory",
          label: "memory",
          kind: null,
          file: "src/memory",
          line: 0,
          symbols: 7,
          degree: 3,
          top: [{ ...SAVE, degree: 6 }],
        },
      ];
      const nodes: GraphOverviewNode[] =
        level === 0
          ? [ENGINE, RUN_TURN_NODE].map((symbol) => ({
              id: symbol.id,
              label: symbol.name,
              kind: symbol.kind,
              file: symbol.file,
              line: symbol.line,
              symbols: 1,
              degree: symbol.degree,
              top: [],
            }))
          : modules;
      const drawn = new Set(nodes.slice(0, limit).map((node) => node.id));
      return {
        level,
        nodes: nodes.slice(0, limit),
        edges: [{ from: "src/agent", to: "src/memory", kind: "calls", count: 4 }].filter(
          (edge) => drawn.has(edge.from) && drawn.has(edge.to),
        ),
        total_nodes: nodes.length,
        truncated: false,
        total_symbols: 12840,
        sampled_symbols: 12840,
      };
    }),
    context: vi.fn(async (symbolId: string) => contexts[symbolId] ?? contexts[ENGINE.id] ?? null as never),
  };
  const transport = { ...base, ...overrides } as GraphTransport;
  mocks.transport = transport;
  return transport;
}

/** Render with a fresh transport, then focus Engine::stats from the list. */
async function renderFocused(transport: GraphTransport = makeTransport()) {
  const rendered = render(GraphView);
  await screen.findByText("Most connected");
  await fireEvent.click(screen.getByRole("button", { name: /Engine::stats/ }));
  await screen.findByRole("img", { name: "Neighbourhood of Engine::stats" });
  return rendered;
}

describe("GraphView", () => {
  beforeEach(() => {
    mocks.transport = null;
    app.update((current) => ({ ...current, indexing: null }));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("shows the status line and the most-connected entry points", async () => {
    const transport = makeTransport();
    render(GraphView);
    await screen.findByText("Most connected");
    expect(screen.getByText("3,945 files · 12,840 symbols · 34,210 edges · indexed 7m ago")).toBeTruthy();
    expect(transport.top).toHaveBeenCalledWith(12);
    const row = screen.getByRole("button", { name: /Engine::stats/ });
    expect(within(row).getByText("method · 42 links")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("polls the status line while the indexer runs", async () => {
    vi.useFakeTimers();
    const transport = makeTransport({
      status: vi.fn(async () => status({ indexing: true, symbols: 1 })),
    });
    render(GraphView);
    await vi.advanceTimersByTimeAsync(0);
    await screen.findByText("Indexing…");
    await vi.advanceTimersByTimeAsync(1500);
    expect(transport.status).toHaveBeenCalledTimes(2);
  });

  it("says code indexing is off when the graph is unavailable", async () => {
    makeTransport({ status: vi.fn(async () => status({ available: false })) });
    render(GraphView);
    await screen.findByText(/Code indexing is off for this project/);
    expect(screen.queryByLabelText("Search symbols by name")).toBeNull();
  });

  it("explains the tab needs the desktop app without a transport", () => {
    mocks.transport = null;
    render(GraphView);
    expect(screen.getByText("The code graph is available in the desktop app.")).toBeTruthy();
  });

  it("debounces search, applies kind filters and returns to the list", async () => {
    vi.useFakeTimers();
    const transport = makeTransport();
    render(GraphView);
    await screen.findByText("Most connected");

    const input = screen.getByLabelText("Search symbols by name");
    await fireEvent.input(input, { target: { value: "eng" } });
    await vi.advanceTimersByTimeAsync(250);
    expect(screen.getByText("1 match")).toBeTruthy();
    expect(transport.search).toHaveBeenCalledWith("eng", null);

    await fireEvent.click(screen.getByRole("button", { name: "structs" }));
    await vi.advanceTimersByTimeAsync(250);
    expect(transport.search).toHaveBeenCalledWith("eng", "struct");
    expect(screen.getByRole("button", { name: "structs" }).getAttribute("aria-pressed")).toBe("true");

    await fireEvent.input(input, { target: { value: "" } });
    await vi.advanceTimersByTimeAsync(250);
    expect(screen.getByText("Most connected")).toBeTruthy();
  });

  it("draws the focused symbol's neighbourhood with callers, callees and impact", async () => {
    const transport = makeTransport();
    await renderFocused(transport);
    expect(transport.neighbourhood).toHaveBeenCalledWith(ENGINE.id, 1);
    expect(transport.context).toHaveBeenCalledWith(ENGINE.id);
    expect(transport.impact).toHaveBeenCalledWith(ENGINE.id);

    const canvas = screen.getByRole("img", { name: "Neighbourhood of Engine::stats" });
    expect(within(canvas).getAllByRole("button")).toHaveLength(3);

    expect(screen.getByRole("heading", { level: 2, name: "Engine::stats" })).toBeTruthy();
    expect(screen.getByText("method")).toBeTruthy();
    expect(screen.getByText("src/headless/engine.rs:680")).toBeTruthy();

    const callers = screen.getByRole("region", { name: "Callers" });
    expect(within(callers).getByRole("button", { name: /run_turn/ })).toBeTruthy();
    const callees = screen.getByRole("region", { name: "Callees" });
    expect(within(callees).getByText("+10 more")).toBeTruthy();

    const impact = screen.getByRole("region", { name: "If you change this" });
    expect(within(impact).getByText("Direct")).toBeTruthy();
    expect(within(impact).getByText("Two hops")).toBeTruthy();
    expect(within(impact).getByText("Wider")).toBeTruthy();
    expect(document.querySelector(".graph-legend")?.textContent).toContain("calls");
  });

  it("refocuses when a caller row is clicked", async () => {
    const transport = makeTransport();
    await renderFocused(transport);
    await fireEvent.click(within(screen.getByRole("region", { name: "Callers" })).getByRole("button", { name: /run_turn/ }));
    await screen.findByRole("img", { name: "Neighbourhood of run_turn" });
    expect(transport.neighbourhood).toHaveBeenCalledWith(RUN_TURN.id, 1);
    expect(screen.getByRole("heading", { level: 2, name: "run_turn" })).toBeTruthy();
  });

  it("re-fetches the neighbourhood at depth 2 when the toggle is pressed", async () => {
    const transport = makeTransport();
    await renderFocused(transport);
    const toggle = screen.getByRole("button", { name: "Two hops" });
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
    await fireEvent.click(toggle);
    await screen.findByRole("img", { name: "Neighbourhood of Engine::stats" });
    expect(transport.neighbourhood).toHaveBeenCalledWith(ENGINE.id, 2);
    expect(screen.getByRole("button", { name: "Two hops" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("shows the backend's error when the symbol is gone", async () => {
    makeTransport({
      neighbourhood: vi.fn(async () => Promise.reject("unknown symbol src/old.rs::gone::1")),
    });
    render(GraphView);
    await screen.findByText("Most connected");
    await fireEvent.click(screen.getByRole("button", { name: /Engine::stats/ }));
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.getByText(/unknown symbol src\/old\.rs::gone::1/)).toBeTruthy();
    expect(screen.getByText(/The index may have moved on/)).toBeTruthy();
  });

  it("copies the focused symbol's file:line", async () => {
    await renderFocused();
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    await fireEvent.click(screen.getByRole("button", { name: "Copy location" }));
    expect(writeText).toHaveBeenCalledWith("src/headless/engine.rs:680");
  });

  it("zooms on the wheel and resets the view", async () => {
    await renderFocused();
    const canvas = screen.getByRole("img", { name: "Neighbourhood of Engine::stats" });
    const group = canvas.querySelector("g");
    const fitted = group?.getAttribute("transform") ?? "";
    const zoomOf = () => Number(/scale\(([\d.]+)\)/.exec(group?.getAttribute("transform") ?? "")?.[1]);
    // Opening a view fits it to the canvas rather than showing an empty box.
    expect(zoomOf()).toBeGreaterThan(0);
    await fireEvent.wheel(canvas, { deltaY: -100 });
    expect(zoomOf()).toBeGreaterThan(1);
    await fireEvent.click(screen.getByRole("button", { name: "Reset view" }));
    expect(group?.getAttribute("transform")).toBe(fitted);
  });

  it("drags a node without refocusing it", async () => {
    const transport = makeTransport();
    await renderFocused(transport);
    const canvas = screen.getByRole("img", { name: "Neighbourhood of Engine::stats" });
    const node = within(canvas).getByRole("button", { name: "Focus run_turn" });
    const before = node.getAttribute("transform");
    await fireEvent.pointerDown(node, { pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(canvas, { pointerId: 1, clientX: 130, clientY: 104 });
    await fireEvent.pointerUp(canvas, { pointerId: 1 });
    expect(node.getAttribute("transform")).not.toBe(before);
    expect(transport.neighbourhood).toHaveBeenCalledTimes(1);
  });

  it("refreshes the index from the header", async () => {
    const transport = makeTransport();
    render(GraphView);
    await screen.findByText("Most connected");
    await fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await screen.findByText("Indexing…");
    expect(transport.refresh).toHaveBeenCalledTimes(1);
  });
  it("opens on the whole-graph overview with nothing selected", async () => {
    const transport = makeTransport();
    render(GraphView);
    await screen.findByText("Most connected");
    const canvas = await screen.findByRole("img", { name: "Code graph overview" });
    expect(transport.overview).toHaveBeenCalledWith(2, 60);
    expect(within(canvas).getByRole("button", { name: "Open module agent" })).toBeTruthy();
    expect(within(canvas).getByRole("button", { name: "Open module memory" })).toBeTruthy();
    // The old empty canvas is gone: the graph is on screen from the start.
    expect(screen.queryByText(/Pick a symbol on the left/)).toBeNull();
    expect(screen.getByText(/The whole graph: 12,840 symbols in 2 modules/)).toBeTruthy();
  });

  it("draws compact nodes and 10px labels at any view size", async () => {
    await renderFocused();
    const canvas = screen.getByRole("img", { name: "Neighbourhood of Engine::stats" });
    const circles = canvas.querySelectorAll("circle");
    expect(circles.length).toBeGreaterThan(1);
    for (const circle of circles) {
      const radius = Number(circle.getAttribute("r"));
      expect(radius).toBeGreaterThanOrEqual(3);
      expect(radius).toBeLessThanOrEqual(7);
    }
    const labels = canvas.querySelectorAll("text");
    expect(labels.length).toBeGreaterThan(0);
    for (const label of labels) {
      expect(label.getAttribute("style")).toContain("font-size: 10px");
    }
  });

  it("toggles between modules and the top symbols", async () => {
    const transport = makeTransport();
    render(GraphView);
    await screen.findByText("Most connected");
    await screen.findByRole("img", { name: "Code graph overview" });
    await fireEvent.click(screen.getByRole("button", { name: "Top symbols" }));
    const canvas = await screen.findByRole("img", { name: "Code graph overview" });
    expect(transport.overview).toHaveBeenLastCalledWith(0, 40);
    expect(within(canvas).getByRole("button", { name: "Focus run_turn" })).toBeTruthy();
  });

  it("drills into a module and returns to the whole graph", async () => {
    const transport = makeTransport();
    render(GraphView);
    await screen.findByText("Most connected");
    const overview = await screen.findByRole("img", { name: "Code graph overview" });
    await fireEvent.click(within(overview).getByRole("button", { name: "Open module agent" }));
    const drilled = await screen.findByRole("img", { name: "Symbols in agent" });
    expect(within(drilled).getByRole("button", { name: "Focus run_turn" })).toBeTruthy();
    expect(screen.getByText("Symbols")).toBeTruthy();
    // A symbol in the ring focuses through the ordinary neighbourhood path.
    await fireEvent.click(within(drilled).getByRole("button", { name: "Focus run_turn" }));
    await screen.findByRole("img", { name: "Neighbourhood of run_turn" });
    expect(transport.neighbourhood).toHaveBeenCalled();
    await fireEvent.click(screen.getByRole("button", { name: "Whole graph" }));
    await screen.findByRole("img", { name: "Code graph overview" });
  });

  it("says the code index is rebuilding, with file progress", async () => {
    seedCodePass("running");
    makeTransport({ status: vi.fn(async () => status({ indexing: true })) });
    render(GraphView);
    expect(await screen.findByText(/Rebuilding the code index/)).toBeTruthy();
    expect(screen.getByText("1,200/2,400 files")).toBeTruthy();
  });

  it("counts symbols, not files, when the host has no total yet", async () => {
    makeTransport({ status: vi.fn(async () => status({ indexing: true })) });
    render(GraphView);
    expect(await screen.findByText("12,840 symbols so far")).toBeTruthy();
  });

  it("reloads the map, the search and the selection once the pass ends", async () => {
    vi.useFakeTimers();
    let indexing = true;
    const transport = makeTransport({ status: vi.fn(async () => status({ indexing })) });
    render(GraphView);
    await vi.advanceTimersByTimeAsync(0);
    expect(await screen.findByText(/Rebuilding the code index/)).toBeTruthy();
    // A search and a focused symbol, both answered while the pass was running.
    await fireEvent.input(screen.getByLabelText("Search symbols by name"), { target: { value: "eng" } });
    await vi.advanceTimersByTimeAsync(250);
    expect(transport.search).toHaveBeenCalledTimes(1);
    await fireEvent.click(screen.getByRole("button", { name: /Engine::stats/ }));
    await vi.advanceTimersByTimeAsync(0);
    expect(screen.getByRole("img", { name: "Neighbourhood of Engine::stats" })).toBeTruthy();
    indexing = false;
    vi.mocked(transport.search).mockClear();
    await vi.advanceTimersByTimeAsync(1500);
    await vi.advanceTimersByTimeAsync(0);
    // Nothing waited for the user: the map, the query and the ring all reloaded.
    expect(transport.overview).toHaveBeenCalledTimes(2);
    expect(transport.neighbourhood).toHaveBeenCalledTimes(2);
    expect(screen.queryByText(/Rebuilding the code index/)).toBeNull();
    expect(transport.search).toHaveBeenLastCalledWith("eng", null);
  });

  it("calls a symbol missing mid-rebuild 'not indexed yet' and retries when the pass ends", async () => {
    vi.useFakeTimers();
    let indexing = true;
    const transport = makeTransport({ status: vi.fn(async () => status({ indexing })) });
    vi.mocked(transport.neighbourhood).mockRejectedValueOnce(new Error("unknown symbol src/headless/engine.rs::Engine::stats::680"));
    render(GraphView);
    await vi.advanceTimersByTimeAsync(0);
    expect(await screen.findByText("Most connected")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: /Engine::stats/ }));
    expect(await screen.findByText(/Not indexed yet/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
    indexing = false;
    await vi.advanceTimersByTimeAsync(1500);
    await vi.advanceTimersByTimeAsync(0);
    // The completion pass retried the symbol once, and the ring appeared.
    expect(transport.neighbourhood).toHaveBeenCalledTimes(2);
    expect(await screen.findByRole("img", { name: "Neighbourhood of Engine::stats" })).toBeTruthy();
    expect(screen.queryByText(/Not indexed yet/)).toBeNull();
  });

  it("keeps the panel with a note when the finished pass dropped the symbol", async () => {
    vi.useFakeTimers();
    let indexing = true;
    const transport = makeTransport({ status: vi.fn(async () => status({ indexing })) });
    render(GraphView);
    await vi.advanceTimersByTimeAsync(0);
    expect(await screen.findByText("Most connected")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: /Engine::stats/ }));
    await vi.advanceTimersByTimeAsync(0);
    expect(screen.getByRole("img", { name: "Neighbourhood of Engine::stats" })).toBeTruthy();
    // The rebuilt index no longer has the symbol: the retry misses, the panel stays.
    vi.mocked(transport.neighbourhood).mockRejectedValue(new Error("unknown symbol src/headless/engine.rs::Engine::stats::680"));
    indexing = false;
    await vi.advanceTimersByTimeAsync(1500);
    await vi.advanceTimersByTimeAsync(0);
    expect(await screen.findByText(/moved or was removed/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("heading", { name: "Engine::stats" })).toBeTruthy();
  });
});