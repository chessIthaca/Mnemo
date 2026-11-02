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

/** One node of the whole-graph overview: a directory module, or a symbol. */
export interface GraphOverviewNode {
  /** Module directory path, or the symbol id at level 0. */
  id: string;
  /** Short label: the directory's last segment, or the symbol name. */
  label: string;
  /** Symbol kind at level 0; null for a directory module. */
  kind: string | null;
  /** Project-relative file (symbols) or directory path (modules). */
  file: string;
  /** Definition start line (symbols); 0 for a module. */
  line: number;
  /** Symbols the node stands for (1 for a symbol node). */
  symbols: number;
  /** Aggregated edge weight touching the node. */
  degree: number;
  /** Modules: their most-connected symbols, for the drill-in ring. */
  top: GraphNode[];
}

/** One counted, aggregated edge between two overview nodes. */
export interface GraphOverviewEdge {
  from: string;
  to: string;
  /** `calls` / `imports` / `contains`. */
  kind: string;
  /** How many symbol edges this row stands for. */
  count: number;
}

/** The graph's opening view: directory modules (level ≥ 1) or symbols (level 0). */
export interface GraphOverview {
  /** Directory depth aggregated at; 0 is the symbol level. */
  level: number;
  nodes: GraphOverviewNode[];
  edges: GraphOverviewEdge[];
  /** Candidate nodes before the cap. */
  total_nodes: number;
  /** True when the cap dropped nodes. */
  truncated: boolean;
  /** Symbols in the whole graph snapshot. */
  total_symbols: number;
  /** Symbols this overview aggregated (the degree-ranked slice). */
  sampled_symbols: number;
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
  /** The whole-graph overview: directory modules, or the symbol slice at level 0. */
  overview(level?: number, limit?: number): Promise<GraphOverview>;
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
    overview: (level, limit) =>
      bindings.invoke<GraphOverview>("graph_overview", { level: level ?? null, limit: limit ?? null }),
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

/** Compact on-screen node radius: 3px for a leaf. */
export const NODE_RADIUS_MIN = 3;
/** Largest on-screen node radius, in CSS pixels. */
export const NODE_RADIUS_MAX = 7;
/** On-screen label size; a constant, so the zoom never inflates the text. */
export const LABEL_FONT_PX = 10;

/**
 * Compact node radius in CSS pixels: 3px for a leaf, 7px for a hub.
 *
 * The radius is a *screen* size, not a canvas-unit one: the view counter-scales
 * each node group by the current zoom and viewport scale, so a hub never
 * swallows its neighbours and the drawing stays legible with 5 nodes or 300.
 */
export function compactNodeRadius(weight: number, max: number = NODE_RADIUS_MAX): number {
  const scaled = NODE_RADIUS_MIN + Math.sqrt(Math.max(0, weight)) * 0.55;
  return Math.min(max, scaled);
}

/** Circle radius for a neighbourhood node: hubs grow with degree, the centre is emphasised. */
function nodeRadius(symbol: GraphNode, centerId: string): number {
  return symbol.id === centerId ? NODE_RADIUS_MAX : compactNodeRadius(symbol.degree);
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
/** One node a whole-graph layout is computed for. */
export interface LayoutNode {
  id: string;
  /** Compact on-screen radius, in CSS pixels. */
  radius: number;
}

/** One edge a whole-graph layout is computed for. */
export interface LayoutEdge {
  from: string;
  to: string;
}

/**
 * Lay the whole-graph overview out on a `width × height` canvas: there is no
 * centre to pin, so the nodes start on a golden-angle sunflower (even coverage
 * from the first tick) and the same synchronous d3-force pass the
 * neighbourhood uses refines them. Deterministic — same input, same output.
 */
export function computeOverviewLayout(
  nodes: readonly LayoutNode[],
  edges: readonly LayoutEdge[],
  width: number,
  height: number,
): GraphLayout {
  const positions: Record<string, { x: number; y: number }> = {};
  const radii: Record<string, number> = {};
  if (nodes.length === 0) return { positions, radii };

  const ids = new Set(nodes.map((node) => node.id));
  for (const node of nodes) radii[node.id] = node.radius;

  const cx = width / 2;
  const cy = height / 2;
  const spread = Math.min(width, height) * 0.44;
  const goldenAngle = Math.PI * (3 - Math.sqrt(5));
  const simNodes: SimNode[] = nodes.map((node, index) => {
    const angle = index * goldenAngle;
    const distance = spread * Math.sqrt((index + 0.5) / nodes.length);
    return {
      id: node.id,
      x: cx + Math.cos(angle) * distance,
      y: cy + Math.sin(angle) * distance,
    };
  });

  const links: SimLink[] = [];
  for (const edge of edges) {
    if (edge.from === edge.to) continue;
    if (!ids.has(edge.from) || !ids.has(edge.to)) continue;
    links.push({ source: edge.from, target: edge.to });
  }

  // 300 symbols need much shorter springs than 40 modules to stay on canvas.
  const crowded = nodes.length > 120;
  const radiusOf = new Map(nodes.map((node) => [node.id, node.radius]));
  const simulation = forceSimulation<SimNode>(simNodes)
    .force(
      "link",
      forceLink<SimNode, SimLink>(links)
        .id((node) => node.id)
        .distance(crowded ? 44 : 64)
        .strength(0.35),
    )
    .force("charge", forceManyBody<SimNode>().strength(crowded ? -70 : -170))
    .force(
      "collide",
      forceCollide<SimNode>()
        .radius((node) => (radiusOf.get(node.id) ?? NODE_RADIUS_MIN) + (crowded ? 5 : 9))
        .iterations(2),
    )
    .stop();
  for (let tick = 0; tick < LAYOUT_TICKS; tick += 1) simulation.tick();

  for (const node of simNodes) {
    const x = typeof node.x === "number" && Number.isFinite(node.x) ? node.x : cx;
    const y = typeof node.y === "number" && Number.isFinite(node.y) ? node.y : cy;
    positions[node.id] = { x, y };
  }
  return { positions, radii };
}

/** One candidate label, positioned in on-screen pixels. */
export interface LabelCandidate {
  id: string;
  /** Label text, already shortened. */
  text: string;
  /** Node centre, in CSS pixels. */
  x: number;
  y: number;
  /** Node radius, in CSS pixels. */
  radius: number;
  /** 0 is drawn first (selected, hovered); larger values lose the budget. */
  priority: number;
}

interface LabelBox {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** Rough advance width of one 10px label character, in CSS pixels. */
const LABEL_CHAR_PX = 5.6;
/** Label line height, in CSS pixels. */
const LABEL_LINE_PX = 11;

/**
 * Pick the labels that fit without piling up: placed in priority order (the
 * selected node, its neighbours and the hovered node first, then by size),
 * each under its node, skipping any that would touch a placed label or leave
 * the canvas. `budget` caps how many labels one zoom level may draw, so a
 * dense overview shows only its biggest nodes until the user zooms in.
 */
export function visibleLabels(
  candidates: readonly LabelCandidate[],
  width: number,
  height: number,
  budget: number,
): Set<string> {
  const shown = new Set<string>();
  const placed: LabelBox[] = [];
  const ordered = [...candidates].sort((a, b) => a.priority - b.priority || a.id.localeCompare(b.id));
  for (const candidate of ordered) {
    if (shown.size >= budget) break;
    const textWidth = Math.max(8, candidate.text.length * LABEL_CHAR_PX);
    const left = candidate.x - textWidth / 2;
    const top = candidate.y + candidate.radius + 2;
    const box: LabelBox = { left, top, right: left + textWidth, bottom: top + LABEL_LINE_PX };
    if (box.left < 0 || box.right > width || box.bottom > height) continue;
    const collides = placed.some(
      (other) =>
        box.left < other.right &&
        other.left < box.right &&
        box.top < other.bottom &&
        other.top < box.bottom,
    );
    if (collides) continue;
    placed.push(box);
    shown.add(candidate.id);
  }
  return shown;
}

/** The canvas transform: `translate(x y) scale(zoom)`. */
export interface ViewTransform {
  zoom: number;
  x: number;
  y: number;
}

/**
 * Fit a laid-out graph into the `width × height` canvas: the zoom and pan that
 * centre it with a little margin, clamped so a two-node ring does not fill the
 * screen. Called on load and on every view switch, so the whole graph is on
 * screen the moment it is drawn.
 */
export function fitTransform(
  layout: GraphLayout,
  width: number,
  height: number,
  margin = 28,
): ViewTransform {
  const entries = Object.entries(layout.positions);
  if (entries.length === 0) return { zoom: 1, x: 0, y: 0 };
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const [id, at] of entries) {
    const radius = layout.radii[id] ?? NODE_RADIUS_MIN;
    minX = Math.min(minX, at.x - radius);
    minY = Math.min(minY, at.y - radius);
    maxX = Math.max(maxX, at.x + radius);
    maxY = Math.max(maxY, at.y + radius);
  }
  const boxWidth = Math.max(1, maxX - minX);
  const boxHeight = Math.max(1, maxY - minY);
  const zoom = Math.min(
    2.4,
    Math.max(0.4, Math.min((width - margin * 2) / boxWidth, (height - margin * 2) / boxHeight)),
  );
  const centreX = (minX + maxX) / 2;
  const centreY = (minY + maxY) / 2;
  return { zoom, x: width / 2 - zoom * centreX, y: height / 2 - zoom * centreY };
}
