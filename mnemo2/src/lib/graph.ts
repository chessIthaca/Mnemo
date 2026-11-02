import { invoke } from "@tauri-apps/api/core";
import {
  forceCollide,
  forceLink,
  forceManyBody,
  forceSimulation,
  type SimulationLinkDatum,
  type SimulationNodeDatum,
} from "d3-force";
import { isTauriRuntime } from "./runtime";

/**
 * The code-graph tab's transport: one method per `graph_*` Tauri command in
 * `src-tauri/src/mnemo2/graph.rs`. Wire fields are snake_case (serde).
 */

/** Live indexer state. `last_indexed_at` is unix seconds, null when never. */
export interface GraphStatus {
  available: boolean;
  indexing: boolean;
  files: number;
  symbols: number;
  edges: number;
  last_indexed_at: number | null;
}

/** One symbol as the tab renders it: a search hit, node or impact row. */
export interface GraphSymbol {
  /** Stable id (`{file}::{name}::{line}`) — what every other command takes. */
  id: string;
  name: string;
  /** The graph's canonical lowercase kind (`function`, `struct`, …). */
  kind: string;
  file: string;
  /** The definition's start line. */
  line: number;
}

/** A search hit plus its connection count (used for the entry-point list). */
export interface GraphNode extends GraphSymbol {
  degree: number;
}

/** A directed edge between two symbols of the same neighbourhood. */
export interface GraphEdge {
  from: string;
  to: string;
  kind: string;
}

/** One row of "what a change to this symbol touches". */
export interface GraphImpactSymbol extends GraphSymbol {
  /** Hops from the queried symbol; 1 = direct dependant. */
  distance: number;
}

/** The selected symbol plus its drawn neighbourhood. */
export interface GraphNeighbourhood {
  center: GraphNode;
  nodes: GraphNode[];
  edges: GraphEdge[];
  /** True when the node cap kept the closest `nodes` only. */
  truncated: boolean;
}

/** The details panel's exact caller/callee lists. */
export interface GraphContext {
  symbol: GraphNode;
  callers: GraphSymbol[];
  callees: GraphSymbol[];
  caller_total: number;
  callee_total: number;
}

export interface GraphTransport {
  status(): Promise<GraphStatus>;
  /** Trigger a re-index and resolve to the (likely `indexing`) status. */
  refresh(): Promise<GraphStatus>;
  search(query: string, kind: string | null, limit?: number): Promise<GraphSymbol[]>;
  neighbourhood(symbolId: string, depth?: number): Promise<GraphNeighbourhood>;
  impact(symbolId: string): Promise<GraphImpactSymbol[]>;
  /** The most-connected symbols — the tab's entry points. */
  top(limit?: number): Promise<GraphNode[]>;
  context(symbolId: string): Promise<GraphContext>;
}

export interface GraphTransportBindings {
  invoke: typeof invoke;
}

/** The stand-in transport planted for the preview page (see `seedGraphPreview`). */
let previewSeed: GraphTransport | null = null;

/**
 * Dev-only seam for the preview page: plant a stand-in transport so the
 * `graph` scene renders sample data in a plain browser, where no Tauri host
 * exists. Ignored inside the desktop app — `createGraphTransport` only
 * consults the seed when there is no Tauri runtime, so production can never
 * serve planted data.
 */
export function seedGraphPreview(transport: GraphTransport): void {
  previewSeed = transport;
}

function createTauriGraphTransport(bindings: GraphTransportBindings): GraphTransport {
  return {
    status: () => bindings.invoke<GraphStatus>("graph_status"),
    refresh: () => bindings.invoke<GraphStatus>("graph_refresh"),
    search: (query, kind, limit) =>
      bindings.invoke<GraphSymbol[]>("graph_search", { query, kind: kind ?? null, limit: limit ?? null }),
    neighbourhood: (symbolId, depth = 1) =>
      bindings.invoke<GraphNeighbourhood>("graph_neighbourhood", { symbolId, depth }),
    impact: (symbolId) => bindings.invoke<GraphImpactSymbol[]>("graph_impact", { symbolId }),
    top: (limit) => bindings.invoke<GraphNode[]>("graph_top", { limit: limit ?? null }),
    context: (symbolId) => bindings.invoke<GraphContext>("graph_context", { symbolId }),
  };
}

/**
 * The transport for the Graph tab: the real Tauri transport in the desktop
 * app, the planted preview seed in a plain browser (dev preview, tests), or
 * `null` when a browser session has nothing planted — the view then explains
 * the tab needs the desktop app.
 */
export function createGraphTransport(bindings?: GraphTransportBindings): GraphTransport | null {
  if (bindings) return createTauriGraphTransport(bindings);
  if (isTauriRuntime()) return createTauriGraphTransport({ invoke });
  return previewSeed;
}

/** Flatten a rejected invoke (string or Error) to display text. */
export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return "Something went wrong";
}

/** Friendly label for a wire kind; unknown kinds fall back to the raw string. */
export function symbolKindLabel(kind: string): string {
  const labels: Record<string, string> = {
    function: "function",
    method: "method",
    struct: "struct",
    enum: "enum",
    trait: "trait",
    impl: "impl block",
    type_alias: "type alias",
    class: "class",
    interface: "interface",
    ts_enum: "enum",
    module: "module",
    macro_rules: "macro",
  };
  return labels[kind] ?? kind;
}

/** The kind filter chips beside the search box (no chip = every kind). */
export const KIND_FILTERS: readonly { kind: string; label: string }[] = [
  { kind: "function", label: "functions" },
  { kind: "method", label: "methods" },
  { kind: "struct", label: "structs" },
  { kind: "enum", label: "enums" },
  { kind: "module", label: "modules" },
];

/** The legend's edge kinds, in display order. */
export const EDGE_KINDS: readonly { kind: string; label: string }[] = [
  { kind: "calls", label: "calls" },
  { kind: "imports", label: "imports" },
  { kind: "contains", label: "contains" },
];

/** Group an integer with thin thousands separators for the status line. */
export function formatCount(value: number): string {
  const count = Number.isFinite(value) ? Math.max(0, Math.trunc(value)) : 0;
  return count.toLocaleString("en-US");
}

/** "7m ago" style stamp for a unix-seconds value; null is "not indexed yet". */
export function formatRelativeTime(unixSeconds: number | null, nowMs: number = Date.now()): string {
  if (unixSeconds === null || !Number.isFinite(unixSeconds)) return "not indexed yet";
  const seconds = Math.max(0, Math.floor(nowMs / 1000) - unixSeconds);
  if (seconds < 45) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days}d ago`;
  return "over a month ago";
}

/** A node label short enough for the graph: long names get an ellipsis. */
export function nodeLabel(name: string, max = 20): string {
  return name.length <= max ? name : `${name.slice(0, Math.max(1, max - 1))}…`;
}

/** One distance group of the "If you change this" panel. */
export interface ImpactGroup {
  label: string;
  rows: GraphImpactSymbol[];
}

/** Bucket impact rows into Direct / Two hops / Wider, dropping empty buckets. */
export function groupImpact(impact: GraphImpactSymbol[]): ImpactGroup[] {
  const direct: GraphImpactSymbol[] = [];
  const two: GraphImpactSymbol[] = [];
  const wider: GraphImpactSymbol[] = [];
  for (const row of impact) {
    if (row.distance <= 1) direct.push(row);
    else if (row.distance === 2) two.push(row);
    else wider.push(row);
  }
  return [
    { label: "Direct", rows: direct },
    { label: "Two hops", rows: two },
    { label: "Wider", rows: wider },
  ].filter((group) => group.rows.length > 0);
}

/** Layout output: a position and radius per node id. */
export interface GraphLayout {
  positions: Record<string, { x: number; y: number }>;
  radii: Record<string, number>;
}

interface SimNode extends SimulationNodeDatum {
  id: string;
}

interface SimLink extends SimulationLinkDatum<SimNode> {}

/** Circle radius for a node: hubs grow with degree, the centre is emphasised. */
function nodeRadius(symbol: GraphNode, centerId: string): number {
  if (symbol.id === centerId) return 12;
  return 5 + Math.min(8, Math.round(Math.sqrt(Math.max(0, symbol.degree)) * 1.6));
}

const LAYOUT_TICKS = 240;

/**
 * Lay one neighbourhood out on a `width × height` canvas with a synchronous
 * d3-force pass: the centre pinned mid-canvas, its ring placed on a circle and
 * refined by link, charge and collide forces. Deterministic — same input,
 * same output — so the SVG, the preview and tests all agree.
 */
export function computeGraphLayout(
  hood: Pick<GraphNeighbourhood, "center" | "nodes" | "edges">,
  width: number,
  height: number,
): GraphLayout {
  const centerId = hood.center.id;
  const byId = new Map<string, GraphNode>();
  for (const node of [hood.center, ...hood.nodes]) byId.set(node.id, node);

  const simNodes: SimNode[] = [];
  const positions: Record<string, { x: number; y: number }> = {};
  const radii: Record<string, number> = {};

  const ring = [...byId.values()].filter((node) => node.id !== centerId).sort((a, b) => a.id.localeCompare(b.id));
  const ringRadius = Math.min(width, height) * 0.32;
  simNodes.push({ id: centerId, x: width / 2, y: height / 2, fx: width / 2, fy: height / 2 });
  ring.forEach((node, index) => {
    const angle = (2 * Math.PI * index) / Math.max(1, ring.length) + Math.PI / 7;
    simNodes.push({
      id: node.id,
      x: width / 2 + Math.cos(angle) * ringRadius,
      y: height / 2 + Math.sin(angle) * ringRadius,
    });
  });
  for (const node of byId.values()) radii[node.id] = nodeRadius(node, centerId);

  const links: SimLink[] = [];
  for (const edge of hood.edges) {
    if (edge.from === edge.to) continue;
    if (!byId.has(edge.from) || !byId.has(edge.to)) continue;
    links.push({ source: edge.from, target: edge.to });
  }

  const radiusOf = new Map(simNodes.map((node) => [node.id, radii[node.id]]));
  const simulation = forceSimulation<SimNode>(simNodes)
    .force(
      "link",
      forceLink<SimNode, SimLink>(links)
        .id((node) => node.id)
        .distance(78)
        .strength(0.55),
    )
    .force("charge", forceManyBody<SimNode>().strength(-190))
    .force(
      "collide",
      forceCollide<SimNode>()
        .radius((node) => (radiusOf.get(node.id) ?? 6) + 6)
        .iterations(2),
    )
    .stop();
  for (let tick = 0; tick < LAYOUT_TICKS; tick += 1) simulation.tick();

  for (const node of simNodes) {
    const x = typeof node.x === "number" && Number.isFinite(node.x) ? node.x : width / 2;
    const y = typeof node.y === "number" && Number.isFinite(node.y) ? node.y : height / 2;
    positions[node.id] = { x, y };
  }
  return { positions, radii };
}