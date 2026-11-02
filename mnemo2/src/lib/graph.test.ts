import { beforeEach, describe, expect, it, vi } from "vitest";
import { isTauriRuntime } from "./runtime";

vi.mock("./runtime", () => ({ isTauriRuntime: vi.fn(() => false) }));

import {
  compactNodeRadius,
  computeGraphLayout,
  computeOverviewLayout,
  createGraphTransport,
  errorText,
  fitTransform,
  formatCount,
  formatRelativeTime,
  groupImpact,
  NODE_RADIUS_MAX,
  NODE_RADIUS_MIN,
  nodeLabel,
  seedGraphPreview,
  symbolKindLabel,
  visibleLabels,
  type GraphEdge,
  type GraphImpactSymbol,
  type GraphNode,
  type GraphStatus,
  type GraphSymbol,
  type GraphTransport,
} from "./graph";

/** Point `isTauriRuntime` for the current test. */
function setTauri(value: boolean) {
  vi.mocked(isTauriRuntime).mockReturnValue(value);
}

function status(overrides: Partial<GraphStatus> = {}): GraphStatus {
  return {
    available: true,
    indexing: false,
    files: 12,
    symbols: 300,
    edges: 480,
    last_indexed_at: 1_800_000_000,
    ...overrides,
  };
}

function symbol(id: string, name: string, kind = "function", line = 10): GraphSymbol {
  const [file] = id.split("::");
  return { id, name, kind, file, line };
}

function node(id: string, name: string, kind = "function", degree = 3): GraphNode {
  return { ...symbol(id, name, kind), degree };
}

describe("graph transport", () => {
  beforeEach(() => {
    setTauri(false);
  });

  it("returns null in a plain browser with nothing planted", () => {
    expect(createGraphTransport()).toBeNull();
  });

  it("serves the planted preview seed outside Tauri, never inside it", async () => {
    const planted: GraphTransport = {
      status: vi.fn(async () => status()),
      refresh: vi.fn(async () => status({ indexing: true })),
      search: vi.fn(async () => []),
      neighbourhood: vi.fn(async () => null as never),
      impact: vi.fn(async () => []),
      top: vi.fn(async () => []),
      overview: vi.fn(async () => ({
        level: 2,
        nodes: [],
        edges: [],
        total_nodes: 0,
        truncated: false,
        total_symbols: 0,
        sampled_symbols: 0,
      })),
      context: vi.fn(async () => null as never),
    };
    seedGraphPreview(planted);
    expect(createGraphTransport()).toBe(planted);
    await expect(createGraphTransport()?.status()).resolves.toEqual(status());

    setTauri(true);
    const native = createGraphTransport();
    expect(native).not.toBe(planted);
    setTauri(false);
  });

  it("builds a Tauri transport from bindings outside Tauri", async () => {
    const invoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
      if (command === "graph_status") return status({ files: 7 });
      return null;
    });
    const transport = createGraphTransport({
      invoke: invoke as unknown as typeof import("@tauri-apps/api/core").invoke,
    });
    expect(transport).not.toBeNull();
    await expect(transport?.status()).resolves.toEqual(status({ files: 7 }));
  });

  it("maps every method to its command with camelCase arguments", async () => {
    const invoked: { command: string; args?: Record<string, unknown> }[] = [];
    const invoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
      invoked.push({ command, args });
      if (command === "graph_search") return [symbol("a.rs::alpha::1", "alpha")];
      if (command === "graph_neighbourhood") return { center: node("a.rs::alpha::1", "alpha"), nodes: [], edges: [], truncated: false };
      if (command === "graph_impact") return [];
      if (command === "graph_top") return [node("a.rs::alpha::1", "alpha")];
      if (command === "graph_context") {
        return { symbol: node("a.rs::alpha::1", "alpha"), callers: [], callees: [], caller_total: 0, callee_total: 0 };
      }
      return status();
    });
    const transport = createGraphTransport({ invoke: invoke as unknown as typeof import("@tauri-apps/api/core").invoke });

    await transport?.status();
    await transport?.refresh();
    await transport?.search("alpha", null);
    await transport?.search("alpha", "struct", 12);
    await transport?.neighbourhood("a.rs::alpha::1");
    await transport?.neighbourhood("a.rs::alpha::1", 2);
    await transport?.impact("a.rs::alpha::1");
    await transport?.top();
    await transport?.top(12);
    await transport?.context("a.rs::alpha::1");

    const argsOf = (command: string, nth = 1) => invoked.filter((call) => call.command === command)[nth - 1]?.args;
    expect(invoked.map((call) => call.command)).toEqual([
      "graph_status",
      "graph_refresh",
      "graph_search",
      "graph_search",
      "graph_neighbourhood",
      "graph_neighbourhood",
      "graph_impact",
      "graph_top",
      "graph_top",
      "graph_context",
    ]);
    expect(argsOf("graph_search", 1)).toEqual({ query: "alpha", kind: null, limit: null });
    expect(argsOf("graph_search", 2)).toEqual({ query: "alpha", kind: "struct", limit: 12 });
    expect(argsOf("graph_neighbourhood", 1)).toEqual({ symbolId: "a.rs::alpha::1", depth: 1 });
    expect(argsOf("graph_neighbourhood", 2)).toEqual({ symbolId: "a.rs::alpha::1", depth: 2 });
    expect(argsOf("graph_impact")).toEqual({ symbolId: "a.rs::alpha::1" });
    expect(argsOf("graph_top", 1)).toEqual({ limit: null });
    expect(argsOf("graph_top", 2)).toEqual({ limit: 12 });
    expect(argsOf("graph_context")).toEqual({ symbolId: "a.rs::alpha::1" });
  });
});

describe("graph helpers", () => {
  it("flattens rejection reasons to display text", () => {
    expect(errorText("unknown symbol x")).toBe("unknown symbol x");
    expect(errorText(new Error("boom"))).toBe("boom");
    expect(errorText({ message: "wrapped" })).toBe("wrapped");
    expect(errorText(42)).toBe("Something went wrong");
  });

  it("labels known kinds and falls back to raw ones", () => {
    expect(symbolKindLabel("function")).toBe("function");
    expect(symbolKindLabel("type_alias")).toBe("type alias");
    expect(symbolKindLabel("impl")).toBe("impl block");
    expect(symbolKindLabel("ts_enum")).toBe("enum");
    expect(symbolKindLabel("mystery")).toBe("mystery");
  });

  it("groups numbers with thousands separators", () => {
    expect(formatCount(3945)).toBe("3,945");
    expect(formatCount(0)).toBe("0");
    expect(formatCount(Number.NaN)).toBe("0");
  });

  it("formats relative stamps from unix seconds", () => {
    const now = 1_800_000_000_000;
    expect(formatRelativeTime(null, now)).toBe("not indexed yet");
    expect(formatRelativeTime(now / 1000 - 20, now)).toBe("just now");
    expect(formatRelativeTime(now / 1000 - 5 * 60, now)).toBe("5m ago");
    expect(formatRelativeTime(now / 1000 - 3 * 60 * 60, now)).toBe("3h ago");
    expect(formatRelativeTime(now / 1000 - 2 * 86_400, now)).toBe("2d ago");
    expect(formatRelativeTime(now / 1000 - 60 * 86_400, now)).toBe("over a month ago");
  });

  it("truncates long node labels with an ellipsis", () => {
    expect(nodeLabel("short")).toBe("short");
    expect(nodeLabel("record_request_stats_here", 12)).toBe("record_requ…");
  });

  it("groups impact rows by distance and drops empty buckets", () => {
    const rows: GraphImpactSymbol[] = [
      { ...symbol("a.rs::direct::1", "direct"), distance: 1 },
      { ...symbol("b.rs::also_direct::2", "also_direct"), distance: 1 },
      { ...symbol("c.rs::two::3", "two"), distance: 2 },
      { ...symbol("d.rs::wide::4", "wide"), distance: 3 },
      { ...symbol("e.rs::wider::5", "wider"), distance: 5 },
    ];
    const groups = groupImpact(rows);
    expect(groups.map((group) => group.label)).toEqual(["Direct", "Two hops", "Wider"]);
    expect(groups[0].rows).toHaveLength(2);
    expect(groups[1].rows).toHaveLength(1);
    expect(groups[2].rows.map((row) => row.name)).toEqual(["wide", "wider"]);
    expect(groupImpact([])).toEqual([]);
  });
});

describe("computeGraphLayout", () => {
  const edges = (list: [string, string, string][]): GraphEdge[] =>
    list.map(([from, to, kind]) => ({ from, to, kind }));

  it("lays out every node with finite positions and emphasises the centre", () => {
    const center = node("src/engine.rs::Engine::stats::680", "Engine::stats", "method", 40);
    const ring = [
      node("src/agent/turn.rs::run_turn::812", "run_turn", "function", 9),
      node("src/memory/mod.rs::save::30", "save", "method", 6),
      node("src/store.rs::open::4", "open", "function", 3),
      node("src/util.rs::fmt::2", "fmt", "function", 1),
    ];
    const layout = computeGraphLayout(
      { center, nodes: ring, edges: edges([["src/agent/turn.rs::run_turn::812", center.id, "calls"]]) },
      540,
      420,
    );
    const ids = [center.id, ...ring.map((entry) => entry.id)];
    for (const id of ids) {
      expect(Number.isFinite(layout.positions[id].x)).toBe(true);
      expect(Number.isFinite(layout.positions[id].y)).toBe(true);
      expect(layout.radii[id]).toBeGreaterThan(0);
    }
    expect(layout.positions[center.id]).toEqual({ x: 270, y: 210 });
    expect(layout.radii[center.id]).toBe(NODE_RADIUS_MAX);
    expect(NODE_RADIUS_MAX).toBe(7);
    for (const entry of ring) {
      expect(layout.radii[entry.id]).toBeGreaterThanOrEqual(NODE_RADIUS_MIN);
      expect(layout.radii[entry.id]).toBeLessThan(NODE_RADIUS_MAX);
    }
    const distinct = new Set(ring.map((entry) => `${layout.positions[entry.id].x},${layout.positions[entry.id].y}`));
    expect(distinct.size).toBe(ring.length);
  });

  it("is deterministic for the same neighbourhood", () => {
    const center = node("src/a.rs::A::1", "A", "class", 20);
    const ring = "bcdefgh".split("").map((letter, index) => node(`src/${letter}.rs::${letter}::${index}`, letter, "function", index + 1));
    const hood = {
      center,
      nodes: ring,
      edges: edges([
        [ring[0].id, center.id, "calls"],
        [ring[1].id, center.id, "calls"],
        [center.id, ring[2].id, "calls"],
        [center.id, ring[3].id, "imports"],
        [ring[4].id, ring[5].id, "contains"],
        [ring[6].id, center.id, "contains"],
      ]),
    };
    expect(computeGraphLayout(hood, 540, 420)).toEqual(computeGraphLayout(hood, 540, 420));
  });

  it("ignores self edges and edges to nodes that are not drawn", () => {
    const center = node("src/a.rs::A::1", "A", "function", 2);
    const lone = node("src/b.rs::B::2", "B", "function", 1);
    const layout = computeGraphLayout(
      {
        center,
        nodes: [lone],
        edges: edges([
          [center.id, center.id, "calls"],
          [center.id, "src/ghost.rs::Ghost::9", "calls"],
          [lone.id, center.id, "calls"],
        ]),
      },
      540,
      420,
    );
    expect(layout.positions[center.id]).toEqual({ x: 270, y: 210 });
    expect(layout.positions[lone.id]).toBeTruthy();
  });
});
describe("compact node sizing", () => {
  it("keeps every node between 3 and 7 CSS pixels whatever the degree", () => {
    expect(compactNodeRadius(0)).toBe(NODE_RADIUS_MIN);
    expect(compactNodeRadius(1)).toBeGreaterThan(NODE_RADIUS_MIN);
    expect(compactNodeRadius(10_000)).toBe(NODE_RADIUS_MAX);
    for (const weight of [0, 1, 4, 9, 25, 100, 400]) {
      const radius = compactNodeRadius(weight);
      expect(radius).toBeGreaterThanOrEqual(NODE_RADIUS_MIN);
      expect(radius).toBeLessThanOrEqual(NODE_RADIUS_MAX);
    }
    expect(compactNodeRadius(400)).toBeGreaterThan(compactNodeRadius(4));
  });
});

describe("computeOverviewLayout", () => {
  const modules = Array.from({ length: 40 }, (_, index) => ({
    id: `mod-${index}`,
    radius: compactNodeRadius(index),
  }));
  const edges = modules.slice(1).map((entry, index) => ({ from: modules[index].id, to: entry.id }));

  it("places every node on the canvas with finite coordinates", () => {
    const layout = computeOverviewLayout(modules, edges, 540, 420);
    for (const entry of modules) {
      expect(Number.isFinite(layout.positions[entry.id].x)).toBe(true);
      expect(Number.isFinite(layout.positions[entry.id].y)).toBe(true);
      expect(layout.radii[entry.id]).toBe(entry.radius);
    }
    const distinct = new Set(
      modules.map((entry) => `${layout.positions[entry.id].x.toFixed(2)},${layout.positions[entry.id].y.toFixed(2)}`),
    );
    expect(distinct.size).toBe(modules.length);
  });

  it("is deterministic and tolerates dangling edges and empty graphs", () => {
    expect(computeOverviewLayout(modules, edges, 540, 420)).toEqual(
      computeOverviewLayout(modules, edges, 540, 420),
    );
    expect(
      computeOverviewLayout(
        modules.slice(0, 3),
        [{ from: "mod-0", to: "ghost" }, { from: "mod-1", to: "mod-1" }],
        540,
        420,
      ).positions["ghost"],
    ).toBeUndefined();
    expect(computeOverviewLayout([], [], 540, 420)).toEqual({ positions: {}, radii: {} });
  });
});

describe("visibleLabels", () => {
  const candidate = (id: string, x: number, y: number, priority: number) => ({
    id,
    text: id,
    x,
    y,
    radius: NODE_RADIUS_MIN,
    priority,
  });

  it("keeps labels apart and inside the canvas", () => {
    const shown = visibleLabels(
      [
        candidate("a", 100, 100, 0),
        candidate("b", 104, 102, 10),
        candidate("c", 300, 200, 20),
        candidate("off", 539, 419, 30),
      ],
      540,
      420,
      10,
    );
    expect(shown.has("a")).toBe(true);
    expect(shown.has("b")).toBe(false);
    expect(shown.has("c")).toBe(true);
    expect(shown.has("off")).toBe(false);
  });

  it("spends the budget in priority order", () => {
    const shown = visibleLabels(
      [
        candidate("low", 60, 60, 10),
        candidate("hovered", 300, 300, 0),
        candidate("mid", 60, 300, 5),
      ],
      540,
      420,
      2,
    );
    expect([...shown].sort()).toEqual(["hovered", "mid"]);
  });

  it("draws the selected node and its neighbours before the rest", () => {
    const shown = visibleLabels(
      [
        candidate("hub", 60, 60, 1),
        candidate("near", 200, 60, 2),
        candidate("far", 340, 60, 30),
      ],
      540,
      420,
      2,
    );
    expect(shown.has("hub")).toBe(true);
    expect(shown.has("near")).toBe(true);
    expect(shown.has("far")).toBe(false);
  });
});

describe("fitTransform", () => {
  it("centres a layout and keeps it inside the canvas", () => {
    const layout = {
      positions: { a: { x: 0, y: 0 }, b: { x: 200, y: 100 } },
      radii: { a: 5, b: 5 },
    };
    const fit = fitTransform(layout, 540, 420, 28);
    expect(fit.zoom).toBeGreaterThan(1);
    expect(fit.x + fit.zoom * 100).toBeCloseTo(270, 6);
    expect(fit.y + fit.zoom * 50).toBeCloseTo(210, 6);
    // Both endpoints land inside the margins.
    for (const point of [layout.positions.a, layout.positions.b]) {
      expect(fit.x + fit.zoom * point.x).toBeGreaterThanOrEqual(0);
      expect(fit.x + fit.zoom * point.x).toBeLessThanOrEqual(540);
      expect(fit.y + fit.zoom * point.y).toBeGreaterThanOrEqual(0);
      expect(fit.y + fit.zoom * point.y).toBeLessThanOrEqual(420);
    }
  });

  it("clamps a single node and an empty layout", () => {
    expect(fitTransform({ positions: { a: { x: 10, y: 10 } }, radii: { a: 3 } }, 540, 420)).toEqual(
      { zoom: 2.4, x: 270 - 2.4 * 10, y: 210 - 2.4 * 10 },
    );
    expect(fitTransform({ positions: {}, radii: {} }, 540, 420)).toEqual({ zoom: 1, x: 0, y: 0 });
  });
});
