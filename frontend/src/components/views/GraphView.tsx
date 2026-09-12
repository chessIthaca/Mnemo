// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Network, RefreshCw } from "lucide-react";
import {
  forceCenter,
  forceLink,
  forceManyBody,
  forceSimulation,
  type Simulation,
  type SimulationLinkDatum,
  type SimulationNodeDatum,
} from "d3-force";
import {
  codegraphGraph,
  codegraphRefresh,
  codegraphStatus,
  errMsg,
  memoryAccessLog,
} from "../../lib/tauri";
import type {
  CodegraphStatus,
  GraphEdge,
  GraphSymbol,
  MemoryAccessEntry,
} from "../../lib/tauri";
import { SourceEditor } from "../common/SourceEditor";

/**
 * The Graph tab — an interactive explorer over the project's code knowledge
 * graph (CodeGraph). Shows a d3-force visualization of a subgraph (top ~300
 * most-connected symbols unfiltered; name/kind filters; click a node for its
 * focused neighborhood) with:
 *
 * - a status header (files / symbols / edges counts + a Refresh button that
 *   triggers a background re-index, polled while it runs),
 * - node color by symbol kind + edge styling by edge kind, with a legend,
 * - zoom (wheel) + pan (drag) over the SVG viewBox — drag-free nodes v1,
 * - a side panel with the selected node's details + in/out edge counts
 *   within the current slice (its local context/impact), plus a "Browse
 *   source" action that swaps the bottom pane for the shared SourceEditor
 *   scrolled to the symbol's definition line,
 * - a drag-to-resizable bottom pane (height persisted to localStorage)
 *   showing either that source view or the memory-access log.
 *
 * Degrades gracefully: `available: false` shows an "unavailable" notice
 * instead of empty counts; an unavailable graph command surfaces as a
 * message, not a crash.
 */

/** Poll the status on this cadence while an index pass is running (ms). */
const INDEXING_POLL_MS = 1500;

/** Poll the memory-access log on this cadence while the tab is mounted (ms). */
const ACCESS_LOG_POLL_MS = 5000;

/** The canvas's logical viewBox size (the simulation centers in it). */
const CANVAS = 600;

/** The symbol kinds shown in the legend + their node colors. */
const KIND_COLORS: Record<string, string> = {
  function: "#38bdf8", // sky
  method: "#818cf8", // indigo
  struct: "#34d399", // emerald
  enum: "#fbbf24", // amber
  trait: "#f472b6", // pink
  interface: "#f472b6", // pink (shares trait's slot)
  impl: "#a78bfa", // violet
  class: "#34d399", // emerald (shares struct's slot)
  type_alias: "#94a3b8", // slate
  ts_enum: "#fbbf24", // amber (shares enum's slot)
  module: "#64748b", // dark slate
};

/** Node color for a symbol kind (unknown kinds get neutral gray). */
export function kindColor(kind: string): string {
  return KIND_COLORS[kind] ?? "#6b7280";
}

/**
 * The indexing-flag transition predicate for the Graph tab's reload logic:
 * true only on the true→false FALLING edge — an indexing pass just
 * completed, so the store changed under the tab and the visualization is
 * stale. Pure — extracted so the vitest suite can pin the exact transition
 * table (review F1 regression test).
 */
export function shouldReloadOnIndexingEdge(was: boolean, is: boolean): boolean {
  return was && !is;
}

/** Min height (px) for the bottom pane (log or source view). */
const BOTTOM_MIN = 80;
/** localStorage key persisting the bottom pane's height across sessions. */
const BOTTOM_HEIGHT_KEY = "graphview.bottomHeight";
/** Default bottom-pane height (px) — ~6 log lines / ~8 code lines. */
const BOTTOM_DEFAULT = 140;

/**
 * Clamp a bottom-pane height (px) to `[BOTTOM_MIN, total * 0.6]`, rounded.
 * `total` is the Graph tab's rendered height. Pure — extracted so the
 * vitest suite can pin the clamp table for the drag-to-resize handle.
 */
export function clampBottomHeight(px: number, total: number): number {
  return Math.round(Math.max(BOTTOM_MIN, Math.min(total * 0.6, px)));
}

/** Read the persisted bottom-pane height (default when absent/corrupt). */
function storedBottomHeight(): number {
  const v = Number(localStorage.getItem(BOTTOM_HEIGHT_KEY));
  return Number.isFinite(v) && v >= BOTTOM_MIN ? v : BOTTOM_DEFAULT;
}

/**
 * One-line summary of a memory-access entry (the Graph tab's "Memory
 * access" log). Reads render `R [tier] "detail" → N hits` (the hits suffix
 * only when a hit count is present), writes `W [tier] "detail"`; the tier
 * segment is omitted when `null`. Pure — extracted so the vitest suite can
 * pin the exact shapes.
 */
export function formatAccessLine(e: MemoryAccessEntry): string {
  const op = e.op === "write" ? "W" : "R";
  const tier = e.tier ? ` [${e.tier}]` : "";
  const hits = e.op !== "write" && e.hits !== null ? ` → ${e.hits} hits` : "";
  return `${op}${tier} "${e.detail}"${hits}`;
}

/**
 * Whether the Graph tab is in its "code graph unavailable" state — the
 * early-return branch that swaps the explorer for a notice. That branch
 * must STILL render the memory-access section (review B1): the section
 * shows memory-store data, fully independent of codegraph availability, so
 * codegraph-disabled projects (or a failed graph DB) keep the log visible
 * and the 5s poll has a consumer. Pure — pinned by test.
 */
export function isGraphUnavailable(status: CodegraphStatus | null): boolean {
  return status !== null && !status.available;
}

/** The edge kinds + their stroke styles (distinguished beyond color). */
const EDGE_STYLES: Record<string, string> = {
  calls: "stroke-slate-400",
  imports: "stroke-sky-500",
  contains: "stroke-slate-600",
};

/** A simulation node: the symbol plus d3's mutable position/velocity. */
interface SimNode extends SimulationNodeDatum {
  id: string;
  symbol: GraphSymbol;
}

/** A simulation link between two node ids. */
type SimLink = SimulationLinkDatum<SimNode>;

/**
 * The render-ready model built from a graph payload: resolved links (edges
 * whose endpoints are both present — dangling ids are dropped), each node's
 * degree within the slice (in + out), and the node radius (4 + √degree,
 * capped). Pure — extracted so the vitest suite can exercise it.
 */
export interface GraphModel {
  /** Resolved links (from/to index into `nodes`). */
  links: { from: number; to: number; kind: string }[];
  /** Per-node total degree (in + out edges within the slice). */
  degrees: number[];
  /** Per-node render radius. */
  radii: number[];
}

/**
 * Build the render model for a graph payload: resolve edges to node indexes
 * (dangling endpoints dropped), compute per-node degrees, and derive node
 * radii `4 + √degree` capped at 14 (a hub of 100 edges shouldn't swamp the
 * canvas). Pure function of the payload.
 */
export function buildGraphModel(
  nodes: GraphSymbol[],
  edges: GraphEdge[],
): GraphModel {
  const index = new Map<string, number>();
  nodes.forEach((n, i) => index.set(n.id, i));
  const degrees = new Array<number>(nodes.length).fill(0);
  const links: GraphModel["links"] = [];
  for (const e of edges) {
    const from = index.get(e.from_id);
    const to = index.get(e.to_id);
    if (from === undefined || to === undefined) continue;
    links.push({ from, to, kind: e.kind });
    degrees[from] += 1;
    degrees[to] += 1;
  }
  const radii = degrees.map((d) => Math.min(4 + Math.sqrt(d), 14));
  return { links, degrees, radii };
}

/** One entry of the legend: label + color chip. */
function LegendEntry({ color, label }: { color: string; label: string }) {
  return (
    <span className="flex items-center gap-1 text-[10px] text-slate-400">
      <span
        className="inline-block h-2 w-2 rounded-full"
        style={{ backgroundColor: color }}
      />
      {label}
    </span>
  );
}

/** The "Memory access" log section: the store's rolling last-100 reads +
 *  writes, one line each (capped backend-side). Rendered in BOTH the
 *  explorer layout (inside the resizable bottom pane) and the
 *  codegraph-unavailable layout — the data comes from the memory store,
 *  independent of graph availability (review B1). Border/separation is the
 *  CALLER's concern: the resizable pane's splitter provides the hairline in
 *  the explorer layout; the unavailable layout wraps this in a border-t. */
function MemoryAccessSection({ log }: { log: MemoryAccessEntry[] }) {
  return (
    <div className="flex h-full min-h-0 flex-col px-3 py-1">
      <p className="shrink-0 text-[10px] uppercase tracking-wide text-slate-500">
        Memory access — last {log.length} (capped 100)
      </p>
      <div className="min-h-0 flex-1 overflow-y-auto font-mono text-[10px] leading-relaxed">
        {log.length === 0 ? (
          <p className="text-slate-600">no memory accesses yet</p>
        ) : (
          log.map((e, i) => (
            <div key={`${e.at}-${i}`} className="flex gap-2">
              <span className="shrink-0 text-slate-600">
                {new Date(e.at * 1000).toLocaleTimeString()}
              </span>
              <span
                className={`truncate ${
                  e.op === "write" ? "text-emerald-400" : "text-sky-400"
                }`}
                title={formatAccessLine(e)}
              >
                {formatAccessLine(e)}
              </span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

export function GraphView() {
  const [status, setStatus] = useState<CodegraphStatus | null>(null);
  const [nodes, setNodes] = useState<GraphSymbol[]>([]);
  const [edges, setEdges] = useState<GraphEdge[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [nameFilter, setNameFilter] = useState("");
  const [kindFilter, setKindFilter] = useState("");
  const [focus, setFocus] = useState<{ id: string; name: string } | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [accessLog, setAccessLog] = useState<MemoryAccessEntry[]>([]);
  // The browse-to-source target: set by the selected node's "Browse source"
  // button; while set, the bottom pane shows the shared SourceEditor on that
  // file (scrolled to the symbol's line) instead of the memory-access log.
  const [browse, setBrowse] = useState<{ file: string; line: number } | null>(null);
  // Bottom pane height (memory log / source view), drag-to-resize + persisted.
  const [bottomHeight, setBottomHeight] = useState<number>(storedBottomHeight);
  // Bump on every simulation tick so React re-reads ref positions.
  const [, setTick] = useState(0);
  // viewBox pan/zoom state: {x, y, w, h}.
  const [view, setView] = useState({ x: 0, y: 0, w: CANVAS, h: CANVAS });
  const svgRef = useRef<SVGSVGElement | null>(null);
  const simRef = useRef<Simulation<SimNode, SimLink> | null>(null);
  const nodePos = useRef<SimNode[]>([]);
  const dragRef = useRef<{ x: number; y: number; vx: number; vy: number } | null>(
    null,
  );
  // Root flex column — measures the total height for the bottom-pane clamp.
  const containerRef = useRef<HTMLDivElement | null>(null);
  // Tear down an in-flight bottom-pane drag if the tab unmounts mid-drag.
  const bottomDragCleanup = useRef<(() => void) | null>(null);
  useEffect(() => {
    return () => {
      bottomDragCleanup.current?.();
      bottomDragCleanup.current = null;
    };
  }, []);
  // Re-clamp the persisted bottom height against the CURRENT tab height on
  // mount (review N2, 2026-08-20): a height saved on a large window would
  // otherwise render unclamped on a smaller one until the user drags (the
  // 60% clamp only runs during a drag).
  useEffect(() => {
    const total = containerRef.current?.getBoundingClientRect().height ?? 0;
    if (total > 0) {
      setBottomHeight((h) => clampBottomHeight(h, total));
    }
  }, []);

  const model = useMemo(() => buildGraphModel(nodes, edges), [nodes, edges]);

  /** Fetch the graph payload for the current filters. */
  const loadGraph = useCallback(async () => {
    try {
      const g = await codegraphGraph({
        name: nameFilter || null,
        kind: kindFilter || null,
        fromSymbol: focus?.id ?? null,
        depth: focus ? 1 : null,
      });
      setNodes(g.nodes);
      setEdges(g.edges);
      setError(null);
    } catch (e) {
      setError(errMsg(e));
    }
  }, [nameFilter, kindFilter, focus]);

  /** Poll status once + on a cadence while indexing. */
  useEffect(() => {
    let disposed = false;
    const poll = async () => {
      try {
        const s = await codegraphStatus();
        if (!disposed) setStatus(s);
      } catch {
        /* status never errors; ignore transport failures */
      }
    };
    void poll();
    if (status?.indexing) {
      const t = window.setInterval(poll, INDEXING_POLL_MS);
      return () => {
        disposed = true;
        window.clearInterval(t);
      };
    }
    return () => {
      disposed = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status?.indexing]);

  // Poll the memory-access log on a light cadence while the tab is mounted.
  // Reading the log pushes no entries (only write/recall/session-primer do),
  // so this is a pure read of the store's ring — no feedback loop. Load
  // failures keep the last snapshot (transport blips shouldn't blank the
  // section); an unwired store returns an empty log, rendering "no accesses
  // yet".
  useEffect(() => {
    let disposed = false;
    const load = async () => {
      try {
        const log = await memoryAccessLog();
        if (!disposed) setAccessLog(log);
      } catch {
        /* keep the last snapshot */
      }
    };
    void load();
    const t = window.setInterval(() => void load(), ACCESS_LOG_POLL_MS);
    return () => {
      disposed = true;
      window.clearInterval(t);
    };
  }, []);

  // (Re)load the graph whenever the filters change (debounced for typing).
  useEffect(() => {
    const t = window.setTimeout(() => void loadGraph(), 250);
    return () => window.clearTimeout(t);
  }, [loadGraph]);

  // Reload the visualization when the store changes under the tab (review
  // F1 + closing-review NOTE): on the indexing flag's true→false falling
  // edge (a pass just completed), OR on the FIRST available status — the
  // mount-time debounced loadGraph can race a startup pass that completes
  // between the fetch and the first status poll, in which case the poll
  // reports indexing:false with no falling edge to observe. A reload issued
  // after the status response is ordered after any pass that finished before
  // it, so it always sees the post-pass store. (`firstAvailable` is consumed
  // on the first `available: true`; the poll effect's cadence handles
  // in-flight passes via the falling edge as usual.)
  const prevIndexing = useRef(false);
  const firstAvailable = useRef(true);
  useEffect(() => {
    const was = prevIndexing.current;
    const is = status?.indexing ?? false;
    prevIndexing.current = is;
    const first = status?.available === true && firstAvailable.current;
    if (first) firstAvailable.current = false;
    if (shouldReloadOnIndexingEdge(was, is) || first) {
      void loadGraph();
    }
  }, [status?.indexing, loadGraph]);

  // Stop the previous simulation when the payload changes.
  useEffect(() => {
    return () => {
      simRef.current?.stop();
      simRef.current = null;
    };
  }, [nodes]);

  // Run the force simulation over the current payload, writing positions
  // into the node ref; each tick bumps state so React re-renders the SVG.
  useEffect(() => {
    if (nodes.length === 0) {
      nodePos.current = [];
      return;
    }
    const simNodes: SimNode[] = nodes.map((symbol, i) => ({
      id: symbol.id,
      symbol,
      // Seed positions on a circle so the layout starts deterministic.
      x: CANVAS / 2 + 200 * Math.cos((2 * Math.PI * i) / nodes.length),
      y: CANVAS / 2 + 200 * Math.sin((2 * Math.PI * i) / nodes.length),
    }));
    nodePos.current = simNodes;
    const simLinks: SimLink[] = model.links.map((l) => ({
      source: simNodes[l.from],
      target: simNodes[l.to],
    }));
    const sim = forceSimulation(simNodes)
      .force(
        "link",
        forceLink<SimNode, SimLink>(simLinks)
          .id((d) => d.id)
          .distance(50),
      )
      .force("charge", forceManyBody().strength(-120))
      .force("center", forceCenter(CANVAS / 2, CANVAS / 2));
    sim.on("tick", () => setTick((t) => t + 1));
    simRef.current = sim;
    return () => {
      sim.stop();
    };
    // model.links derives from nodes+edges; the ref rebuild is keyed on nodes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nodes, model]);

  // Non-passive wheel handler for zoom (React's onWheel is passive).
  useEffect(() => {
    const el = svgRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      setView((v) => {
        const factor = e.deltaY > 0 ? 1.15 : 1 / 1.15;
        const w = Math.min(Math.max(v.w * factor, CANVAS / 8), CANVAS * 4);
        const h = (w / v.w) * v.h;
        // Zoom about the canvas center.
        const cx = v.x + v.w / 2;
        const cy = v.y + v.h / 2;
        return { x: cx - w / 2, y: cy - h / 2, w, h };
      });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  /** Background drag → pan the viewBox. */
  const onPointerDown = (e: React.PointerEvent<SVGSVGElement>) => {
    dragRef.current = { x: e.clientX, y: e.clientY, vx: view.x, vy: view.y };
    (e.target as Element).setPointerCapture?.(e.pointerId);
  };
  const onPointerMove = (e: React.PointerEvent<SVGSVGElement>) => {
    const d = dragRef.current;
    if (!d) return;
    const rect = svgRef.current?.getBoundingClientRect();
    if (!rect) return;
    const scale = view.w / rect.width;
    setView((v) => ({
      ...v,
      x: d.vx - (e.clientX - d.x) * scale,
      y: d.vy - (e.clientY - d.y) * scale,
    }));
  };
  const onPointerUp = () => {
    dragRef.current = null;
  };

  /** The Refresh button: trigger a re-index + poll it. */
  const onRefresh = async () => {
    setRefreshing(true);
    try {
      const s = await codegraphRefresh();
      setStatus(s);
      await loadGraph();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setRefreshing(false);
    }
  };

  /** Bottom-pane splitter drag: dragging UP grows the pane (it sits at the
   *  bottom). The clamped height is committed to localStorage on drag END
   *  only — no synchronous write per pointermove. */
  const onBottomSplitterDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const startY = e.clientY;
    const startHeight = bottomHeight;
    const total = containerRef.current?.getBoundingClientRect().height ?? 600;
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);

    const onMove = (ev: PointerEvent) => {
      setBottomHeight(clampBottomHeight(startHeight - (ev.clientY - startY), total));
    };
    const endDrag = (ev: PointerEvent) => {
      try {
        handle.releasePointerCapture(ev.pointerId);
      } catch {
        // Capture may already be released (pointercancel) — ignore.
      }
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", endDrag);
      window.removeEventListener("pointercancel", endDrag);
      bottomDragCleanup.current = null;
      // Persist the final height (re-clamped against the same total).
      const finalHeight = clampBottomHeight(startHeight - (ev.clientY - startY), total);
      setBottomHeight(finalHeight);
      localStorage.setItem(BOTTOM_HEIGHT_KEY, String(finalHeight));
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", endDrag);
    window.addEventListener("pointercancel", endDrag);
    bottomDragCleanup.current = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", endDrag);
      window.removeEventListener("pointercancel", endDrag);
    };
  };

  const selectedNode = selected !== null ? nodes.find((n) => n.id === selected) : undefined;
  const selectedIdx = selected !== null ? nodes.findIndex((n) => n.id === selected) : -1;
  const selectedCounts =
    selectedIdx >= 0
      ? {
          out: model.links.filter((l) => l.from === selectedIdx).length,
          in: model.links.filter((l) => l.to === selectedIdx).length,
        }
      : null;

  if (isGraphUnavailable(status)) {
    return (
      <div className="flex h-full flex-col">
        <div className="flex flex-1 flex-col items-center justify-center gap-2 p-4 text-slate-400">
          <Network className="h-8 w-8 opacity-50" />
          <p className="text-sm">Code graph unavailable</p>
          <p className="text-xs opacity-70">
            Disabled in settings ([general] codegraph = false) or the DB failed
            to open.
          </p>
        </div>
        {/* The memory-access log is independent of graph availability —
            keep it visible in this branch too (review B1). No splitter
            here: there is no graph selection to browse, so the pane sits at
            the persisted bottomHeight with its own separation line. The
            explicit height bounds the log (h-full inside an auto parent
            would render unbounded — review L1, 2026-08-20). */}
        <div
          className="shrink-0 overflow-hidden border-t border-slate-800"
          style={{ height: `${bottomHeight}px` }}
        >
          <MemoryAccessSection log={accessLog} />
        </div>
      </div>
    );
  }

  return (
    <div ref={containerRef} className="flex h-full flex-col">
      {/* Status header */}
      <div className="flex items-center gap-2 border-b border-slate-800 px-3 py-2 text-xs text-slate-400">
        {status ? (
          <span className="flex items-center gap-2">
            {status.indexing && (
              <span className="flex items-center gap-1 text-cyan-400">
                <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-cyan-400" />
                Indexing…
              </span>
            )}
            <span>
              {status.files} files · {status.symbols} symbols · {status.edges}{" "}
              edges
            </span>
          </span>
        ) : (
          <span>loading…</span>
        )}
        <button
          className="ml-auto flex items-center gap-1 rounded px-2 py-1 text-slate-300 hover:bg-slate-800 disabled:opacity-50"
          onClick={() => void onRefresh()}
          disabled={refreshing}
          title="Re-index the code graph"
        >
          <RefreshCw className={`h-3 w-3 ${refreshing ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </div>

      {/* Filters */}
      <div className="flex items-center gap-2 border-b border-slate-800 px-3 py-2">
        <input
          className="w-32 rounded border border-slate-700 bg-slate-900 px-2 py-1 text-xs text-slate-200 placeholder:text-slate-500"
          placeholder="filter by name…"
          value={nameFilter}
          onChange={(e) => {
            setNameFilter(e.target.value);
            setFocus(null);
          }}
        />
        <select
          className="rounded border border-slate-700 bg-slate-900 px-1 py-1 text-xs text-slate-200"
          value={kindFilter}
          onChange={(e) => {
            setKindFilter(e.target.value);
            setFocus(null);
          }}
        >
          <option value="">all kinds</option>
          {Object.keys(KIND_COLORS).map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </select>
        {focus && (
          <button
            className="flex items-center gap-1 rounded bg-sky-900/60 px-2 py-1 text-xs text-sky-300 hover:bg-sky-800"
            onClick={() => setFocus(null)}
            title="Clear the focused neighborhood"
          >
            ⊙ {focus.name} ✕
          </button>
        )}
      </div>

      {error && (
        <div className="border-b border-slate-800 bg-red-950/40 px-3 py-1 text-xs text-red-300">
          {error}
        </div>
      )}

      {/* Canvas + side panel */}
      <div className="flex min-h-0 flex-1">
        <svg
          ref={svgRef}
          className="min-w-0 flex-1 cursor-grab touch-none select-none"
          viewBox={`${view.x} ${view.y} ${view.w} ${view.h}`}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerLeave={onPointerUp}
        >
          {model.links.map((l, i) => {
            const from = nodePos.current[l.from];
            const to = nodePos.current[l.to];
            if (!from || !to) return null;
            return (
              <line
                key={i}
                x1={from.x}
                y1={from.y}
                x2={to.x}
                y2={to.y}
                strokeWidth={0.6}
                className={EDGE_STYLES[l.kind] ?? "stroke-slate-500"}
                strokeDasharray={l.kind === "imports" ? "3 3" : l.kind === "contains" ? "1 4" : undefined}
              />
            );
          })}
          {nodePos.current.map((n, i) => {
            // Guard against the transient old-nodePos/new-model misalignment
            // (review F3): between setNodes/setEdges and the simulation
            // effect, nodePos still holds the PREVIOUS payload while
            // model.radii/degrees hold the new one — with a shrunk payload
            // the tail indices are undefined (r={undefined} renders an
            // invisible circle). Mirrors the links guard above; the
            // simulation effect rebuilds nodePos within a frame.
            const r = model.radii[i];
            if (r === undefined) return null;
            return (
              <g
                key={n.id}
                onPointerDown={(e) => e.stopPropagation()}
                onClick={() => setSelected(n.id === selected ? null : n.id)}
                className="cursor-pointer"
              >
                <circle
                  cx={n.x}
                  cy={n.y}
                  r={r}
                  fill={kindColor(n.symbol.kind)}
                  opacity={selected === n.id ? 1 : 0.85}
                  stroke={selected === n.id ? "#e2e8f0" : "none"}
                  strokeWidth={selected === n.id ? 1.5 : 0}
                />
                {model.degrees[i] >= 6 && (
                  <text
                    x={n.x}
                    y={(n.y ?? 0) - r - 2}
                    textAnchor="middle"
                    fontSize={8}
                    fill="#94a3b8"
                  >
                    {n.symbol.name}
                  </text>
                )}
              </g>
            );
          })}
        </svg>

        {/* Selected node panel */}
        {selectedNode && (
          <div className="w-52 shrink-0 overflow-y-auto border-l border-slate-800 p-3 text-xs">
            <p className="mb-1 break-words font-medium text-slate-200">
              {selectedNode.name}
            </p>
            <p className="mb-2 text-slate-500">{selectedNode.kind}</p>
            <p className="mb-3 break-all text-slate-400">
              {selectedNode.file}:{selectedNode.start_line}
            </p>
            {selectedCounts && (
              <div className="mb-3 space-y-1 text-slate-400">
                <p>→ {selectedCounts.out} outgoing</p>
                <p>← {selectedCounts.in} incoming</p>
              </div>
            )}
            <button
              className="w-full rounded bg-sky-900/60 px-2 py-1 text-sky-300 hover:bg-sky-800"
              onClick={() => setFocus({ id: selectedNode.id, name: selectedNode.name })}
            >
              Focus neighborhood
            </button>
            <button
              className="mt-1.5 w-full rounded bg-sky-900/60 px-2 py-1 text-sky-300 hover:bg-sky-800"
              onClick={() =>
                setBrowse({
                  file: selectedNode.file,
                  line: selectedNode.start_line,
                })
              }
              title="Open the file in the bottom pane, scrolled to this symbol's line"
            >
              Browse source ↘
            </button>
          </div>
        )}
      </div>

      {/* Legend */}
      <div className="flex flex-wrap items-center gap-3 border-t border-slate-800 px-3 py-1">
        <LegendEntry color={KIND_COLORS.function} label="function" />
        <LegendEntry color={KIND_COLORS.method} label="method" />
        <LegendEntry color={KIND_COLORS.struct} label="struct/class" />
        <LegendEntry color={KIND_COLORS.trait} label="trait/interface" />
        <LegendEntry color={KIND_COLORS.enum} label="enum" />
        <LegendEntry color={KIND_COLORS.impl} label="impl" />
        <span className="ml-2 text-[10px] text-slate-500">
          — calls · ┄ imports · ⋯ contains
        </span>
      </div>

      {/* Bottom pane — drag-to-resize (height persisted). Shows the shared
          SourceEditor when a node's "Browse source" targeted a file (with a
          ✕ to return to the log), otherwise the memory-access log. The
          splitter's border-y hairlines provide the separation (the section
          itself draws no border); the strip paints the chrome-band
          background (bg-bg-secondary) — the lighter seam the horizontal
          bars always showed, which the vertical handle now matches instead
          of the other way around. */}
      <div
        onPointerDown={onBottomSplitterDown}
        role="separator"
        aria-orientation="horizontal"
        aria-label="Resize bottom area"
        title="Drag to resize the bottom area"
        className="group flex h-1.5 shrink-0 cursor-row-resize items-center justify-center border-y border-border bg-bg-secondary transition-colors hover:bg-cyan-500/20"
      >
        <div className="h-0.5 w-8 rounded-full bg-slate-500 group-hover:bg-slate-400" />
      </div>
      <div className="shrink-0 overflow-hidden" style={{ height: `${bottomHeight}px` }}>
        {browse ? (
          <SourceEditor
            path={browse.file}
            revealLine={browse.line}
            onClose={() => setBrowse(null)}
          />
        ) : (
          <MemoryAccessSection log={accessLog} />
        )}
      </div>
    </div>
  );
}
