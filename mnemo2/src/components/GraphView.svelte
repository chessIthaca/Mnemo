<script lang="ts">
  import { Check, Copy, LoaderCircle, Minus, Network, Plus, RefreshCw, RotateCcw, Search } from "@lucide/svelte";
  import {
    computeGraphLayout,
    createGraphTransport,
    EDGE_KINDS,
    errorText,
    formatCount,
    formatRelativeTime,
    groupImpact,
    KIND_FILTERS,
    nodeLabel,
    symbolKindLabel,
    type GraphContext,
    type GraphImpactSymbol,
    type GraphNeighbourhood,
    type GraphNode,
    type GraphStatus,
    type GraphSymbol,
    type GraphTransport,
  } from "../lib/graph";

  const CANVAS_W = 540;
  const CANVAS_H = 420;
  const SEARCH_DEBOUNCE_MS = 220;
  const INDEX_POLL_MS = 1500;
  const TOP_LIMIT = 12;

  const transport = createGraphTransport();

  let status = $state<GraphStatus | null>(null);
  let statusError = $state("");
  let loadingStatus = $state(false);
  let refreshing = $state(false);

  let query = $state("");
  let kindFilter = $state<string | null>(null);
  let results = $state<GraphSymbol[] | null>(null);
  let searching = $state(false);
  let searchError = $state("");

  let entryPoints = $state<GraphNode[]>([]);

  let selected = $state<GraphSymbol | null>(null);
  let depth = $state<1 | 2>(1);
  let hood = $state<GraphNeighbourhood | null>(null);
  let context = $state<GraphContext | null>(null);
  let impact = $state<GraphImpactSymbol[]>([]);
  let viewError = $state("");
  let loadingView = $state(false);

  /** Node positions in canvas units — the layout's output, drag moves these. */
  let positions = $state<Record<string, { x: number; y: number }>>({});
  let radii = $state<Record<string, number>>({});

  let zoom = $state(1);
  let pan = $state({ x: 0, y: 0 });
  let copied = $state(false);

  let canvasSvg = $state<SVGSVGElement | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  let statusSeq = 0;
  let searchSeq = 0;
  let viewSeq = 0;

  /** The drawn nodes: the centre first, then its ring(s), de-duplicated. */
  const drawnNodes = $derived.by(() => {
    if (!hood) return [] as GraphNode[];
    const seen = new Set<string>();
    const nodes: GraphNode[] = [];
    for (const node of [hood.center, ...hood.nodes]) {
      if (seen.has(node.id)) continue;
      seen.add(node.id);
      nodes.push(node);
    }
    return nodes;
  });

  interface DrawnEdge {
    key: string;
    x1: number;
    y1: number;
    x2: number;
    y2: number;
    kind: string;
  }

  /** The drawn edges with both endpoints resolved to current positions. */
  const drawnEdges = $derived.by(() => {
    if (!hood) return [] as DrawnEdge[];
    const edges: DrawnEdge[] = [];
    for (const edge of hood.edges) {
      const from = positions[edge.from];
      const to = positions[edge.to];
      if (!from || !to) continue;
      edges.push({ key: `${edge.from}\u{2192}${edge.to}:${edge.kind}`, x1: from.x, y1: from.y, x2: to.x, y2: to.y, kind: edge.kind });
    }
    return edges;
  });

  const impactGroups = $derived(groupImpact(impact));

  const statusLine = $derived(
    status
      ? `${formatCount(status.files)} files · ${formatCount(status.symbols)} symbols · ${formatCount(status.edges)} edges · indexed ${formatRelativeTime(status.last_indexed_at)}`
      : "",
  );

  /** Load the status line and the entry-point list; `seq` drops stale answers. */
  function loadStatus(active: GraphTransport) {
    const seq = ++statusSeq;
    loadingStatus = true;
    statusError = "";
    void (async () => {
      // In parallel: the heading and the entry points then appear together,
      // and a rejected `top` (unavailable graph) never loses the status.
      const [statusResult, topResult] = await Promise.allSettled([active.status(), active.top(TOP_LIMIT)]);
      if (seq !== statusSeq) return;
      if (statusResult.status === "fulfilled") status = statusResult.value;
      else statusError = errorText(statusResult.reason);
      if (statusResult.status === "fulfilled" && statusResult.value.available && topResult.status === "fulfilled") {
        entryPoints = topResult.value;
      }
      loadingStatus = false;
    })();
  }

  $effect(() => {
    if (transport) loadStatus(transport);
  });

  /** While the indexer runs, keep the status line (counts, pulse) fresh. */
  $effect(() => {
    const active = transport;
    const indexing = status?.indexing ?? false;
    if (!active || !indexing) return;
    const seq = statusSeq;
    const timer = setInterval(() => {
      void active.status().then((next) => {
        if (seq === statusSeq) status = next;
      });
    }, INDEX_POLL_MS);
    return () => clearInterval(timer);
  });

  /** Debounced search: empty query shows the most-connected entry points. */
  $effect(() => {
    const text = query;
    const kind = kindFilter;
    const active = transport;
    if (!active || !status?.available) return;
    const seq = ++searchSeq;
    const trimmed = text.trim();
    const timer = setTimeout(() => {
      if (seq !== searchSeq) return;
      if (trimmed === "") {
        results = null;
        searching = false;
        searchError = "";
        return;
      }
      searching = true;
      searchError = "";
      void active
        .search(trimmed, kind)
        .then((found) => {
          if (seq === searchSeq) {
            results = found;
            searching = false;
          }
        })
        .catch((error) => {
          if (seq === searchSeq) {
            results = [];
            searchError = errorText(error);
            searching = false;
          }
        });
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  });

  /** Clear the copy tick's timer when the tab goes away. */
  $effect(() => {
    return () => clearTimeout(copyTimer);
  });

  /** Focus a symbol: draw its neighbourhood and load its details. */
  function focus(symbol: GraphSymbol) {
    if (!transport) return;
    if (selected && symbol.id === selected.id && hood) return;
    selected = symbol;
    viewError = "";
    loadingView = true;
    hood = null;
    context = null;
    impact = [];
    positions = {};
    radii = {};
    zoom = 1;
    pan = { x: 0, y: 0 };
    copied = false;
    const seq = ++viewSeq;
    void (async () => {
      try {
        const [hoodLoaded, contextLoaded, impactLoaded] = await Promise.all([
          transport.neighbourhood(symbol.id, depth),
          transport.context(symbol.id),
          transport.impact(symbol.id),
        ]);
        if (seq !== viewSeq) return;
        hood = hoodLoaded;
        context = contextLoaded;
        impact = impactLoaded;
        applyLayout(hoodLoaded);
      } catch (error) {
        if (seq !== viewSeq) return;
        hood = null;
        context = null;
        impact = [];
        positions = {};
        radii = {};
        viewError = errorText(error);
      } finally {
        if (seq === viewSeq) loadingView = false;
      }
    })();
  }

  /** Swap the drawn ring for one or two hops without reloading the details. */
  function setDepth(next: 1 | 2) {
    if (!transport || !selected || next === depth) return;
    depth = next;
    viewError = "";
    const seq = ++viewSeq;
    void transport
      .neighbourhood(selected.id, next)
      .then((hoodLoaded) => {
        if (seq !== viewSeq) return;
        hood = hoodLoaded;
        applyLayout(hoodLoaded);
      })
      .catch((error) => {
        if (seq === viewSeq) viewError = errorText(error);
      });
  }

  function applyLayout(next: GraphNeighbourhood) {
    const layout = computeGraphLayout(next, CANVAS_W, CANVAS_H);
    positions = layout.positions;
    radii = layout.radii;
  }

  /** Re-index now; the poll effect keeps the counts moving until it settles. */
  async function refreshIndex() {
    if (!transport || refreshing) return;
    refreshing = true;
    const seq = ++statusSeq;
    try {
      const next = await transport.refresh();
      if (seq === statusSeq) status = next;
    } catch (error) {
      if (seq === statusSeq) statusError = errorText(error);
    } finally {
      if (seq === statusSeq) refreshing = false;
    }
  }

  /** Copy the selected symbol's `file:line` reference. */
  async function copyLocation() {
    if (!selected) return;
    try {
      await navigator.clipboard?.writeText(`${selected.file}:${selected.line}`);
      copied = true;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copied = false), 1600);
    } catch {
      // Clipboard access denied — the location stays visible to copy by hand.
    }
  }

  type Drag =
    | { kind: "pan"; startX: number; startY: number; origX: number; origY: number }
    | { kind: "node"; id: string; startX: number; startY: number; origX: number; origY: number; moved: boolean };

  let drag: Drag | null = null;
  let clickBlocked = false;

  /** Screen-to-canvas factor: viewBox units per CSS pixel, times the zoom. */
  function dragScale(): number {
    const width = canvasSvg?.clientWidth ?? 0;
    const base = width > 0 ? width / CANVAS_W : 1;
    return base * zoom;
  }

  function onCanvasPointerDown(event: PointerEvent) {
    if (!hood) return;
    drag = { kind: "pan", startX: event.clientX, startY: event.clientY, origX: pan.x, origY: pan.y };
    canvasSvg?.setPointerCapture?.(event.pointerId);
  }

  function onNodePointerDown(event: PointerEvent, id: string) {
    const at = positions[id];
    if (!at) return;
    event.stopPropagation();
    drag = { kind: "node", id, startX: event.clientX, startY: event.clientY, origX: at.x, origY: at.y, moved: false };
    canvasSvg?.setPointerCapture?.(event.pointerId);
  }

  function onCanvasPointerMove(event: PointerEvent) {
    if (!drag) return;
    if (drag.kind === "pan") {
      const scale = dragScale() / zoom;
      pan = { x: drag.origX + (event.clientX - drag.startX) / scale, y: drag.origY + (event.clientY - drag.startY) / scale };
      return;
    }
    const nextX = drag.origX + (event.clientX - drag.startX) / dragScale();
    const nextY = drag.origY + (event.clientY - drag.startY) / dragScale();
    if (Math.abs(nextX - drag.origX) + Math.abs(nextY - drag.origY) > 2) drag.moved = true;
    positions = { ...positions, [drag.id]: { x: nextX, y: nextY } };
  }

  function onCanvasPointerUp() {
    if (drag?.kind === "node") clickBlocked = drag.moved;
    drag = null;
  }

  /** Clicking a node focuses it — unless the click ended a drag. */
  function onNodeClick(id: string) {
    if (clickBlocked) {
      clickBlocked = false;
      return;
    }
    const node = hood?.center.id === id ? hood.center : hood?.nodes.find((entry) => entry.id === id);
    if (node) focus(node);
  }

  function onNodeKey(event: KeyboardEvent, id: string) {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    onNodeClick(id);
  }

  function onCanvasWheel(event: WheelEvent) {
    event.preventDefault();
    zoomBy(event.deltaY < 0 ? 1.15 : 1 / 1.15);
  }

  function zoomBy(factor: number) {
    zoom = Math.min(3, Math.max(0.4, zoom * factor));
  }

  function resetView() {
    zoom = 1;
    pan = { x: 0, y: 0 };
  }
</script>

<section class="graph-view" aria-label="Code graph">
  <header class="graph-header">
    <div class="graph-heading">
      <h1>Code graph</h1>
      {#if transport && status}
        <p class="status-line">
          {statusLine}
          {#if status.indexing}<span class="index-pulse"><i class="pulse-dot"></i>Indexing…</span>{/if}
        </p>
      {:else if transport && loadingStatus}
        <p class="status-line">Loading graph…</p>
      {/if}
    </div>
    {#if transport && status?.available}
      <button class="button button-quiet" onclick={refreshIndex} disabled={refreshing}>
        <span class:spin={refreshing || status.indexing}><RefreshCw size={13} /></span> Refresh
      </button>
    {/if}
  </header>

  {#if !transport}
    <div class="graph-note">
      <Network size={20} />
      <p>The code graph is available in the desktop app.</p>
    </div>
  {:else if !status && loadingStatus}
    <div class="graph-note">
      <span class="spin"><LoaderCircle size={18} /></span>
      <p>Loading the code graph…</p>
    </div>
  {:else if !status && statusError}
    <div class="graph-note">
      <p role="alert">{statusError}</p>
      <button class="button button-quiet" onclick={() => loadStatus(transport)}>Try again</button>
    </div>
  {:else if status && !status.available}
    <div class="graph-note">
      <Network size={20} />
      <p>Code indexing is off for this project — there are no symbols to explore yet.</p>
    </div>
  {:else if status}
    <div class="graph-body">
      <aside class="graph-side" aria-label="Symbol search">
        <div class="search-box">
          <Search size={13} />
          <input
            type="search"
            aria-label="Search symbols by name"
            placeholder="Search symbols by name"
            bind:value={query}
          />
        </div>
        <div class="kind-chips" role="group" aria-label="Filter by symbol kind">
          <button class:active={kindFilter === null} aria-pressed={kindFilter === null} onclick={() => (kindFilter = null)}>All</button>
          {#each KIND_FILTERS as filter (filter.kind)}
            <button
              class:active={kindFilter === filter.kind}
              aria-pressed={kindFilter === filter.kind}
              onclick={() => (kindFilter = kindFilter === filter.kind ? null : filter.kind)}
            >{filter.label}</button>
          {/each}
        </div>
        {#if searchError}
          <p class="side-error" role="alert">{searchError}</p>
        {/if}
        {#if results}
          <p class="side-heading">{results.length === 0 ? "No" : formatCount(results.length)} match{results.length === 1 ? "" : "es"}</p>
          <ul class="symbol-list">
            {#each results as symbol (symbol.id)}
              <li>
                <button class="symbol-row" class:selected={selected?.id === symbol.id} onclick={() => focus(symbol)}>
                  <span class="symbol-name">{symbol.name}</span>
                  <span class="symbol-meta">{symbolKindLabel(symbol.kind)} · {symbol.file}:{symbol.line}</span>
                </button>
              </li>
            {/each}
          </ul>
        {:else if searching}
          <p class="side-heading">Searching…</p>
        {:else}
          <p class="side-heading">Most connected</p>
          <ul class="symbol-list">
            {#each entryPoints as node (node.id)}
              <li>
                <button class="symbol-row" class:selected={selected?.id === node.id} onclick={() => focus(node)}>
                  <span class="symbol-name">{node.name}</span>
                  <span class="symbol-meta">{symbolKindLabel(node.kind)} · {formatCount(node.degree)} links</span>
                </button>
              </li>
            {/each}
          </ul>
          {#if entryPoints.length === 0}<p class="side-empty">Nothing indexed yet — refresh to index this project.</p>{/if}
        {/if}
      </aside>

      <div class="graph-canvas-wrap">
        {#if !selected}
          <div class="canvas-empty">
            <Network size={22} />
            <p>Pick a symbol on the left — the canvas draws its callers and callees around it.</p>
          </div>
        {:else if loadingView}
          <div class="canvas-empty">
            <span class="spin"><LoaderCircle size={18} /></span>
            <p>Loading the neighbourhood…</p>
          </div>
        {:else if viewError}
          <div class="canvas-empty">
            <p role="alert">{viewError}</p>
            <p class="canvas-hint">The index may have moved on — refresh and search again.</p>
          </div>
        {:else if hood}
          <div class="canvas-tools">
            <button class="tool-chip" aria-pressed={depth === 2} onclick={() => setDepth(depth === 2 ? 1 : 2)}>Two hops</button>
            <span class="canvas-spacer"></span>
            <button class="icon-button canvas-zoom" aria-label="Zoom out" onclick={() => zoomBy(1 / 1.25)}><Minus size={13} /></button>
            <button class="icon-button canvas-zoom" aria-label="Zoom in" onclick={() => zoomBy(1.25)}><Plus size={13} /></button>
            <button class="icon-button canvas-zoom" aria-label="Reset view" onclick={resetView}><RotateCcw size={13} /></button>
          </div>
          <svg
            bind:this={canvasSvg}
            class="graph-canvas"
            viewBox="0 0 {CANVAS_W} {CANVAS_H}"
            role="img"
            aria-label="Neighbourhood of {selected.name}"
            onpointerdown={onCanvasPointerDown}
            onpointermove={onCanvasPointerMove}
            onpointerup={onCanvasPointerUp}
            onpointercancel={onCanvasPointerUp}
            onwheel={onCanvasWheel}
          >
            <g transform="translate({pan.x} {pan.y}) scale({zoom})">
              {#each drawnEdges as edge (edge.key)}
                <line class="edge edge-{edge.kind}" x1={edge.x1} y1={edge.y1} x2={edge.x2} y2={edge.y2} />
              {/each}
              {#each drawnNodes as node (node.id)}
                {@const at = positions[node.id]}
                {#if at}
                  <g
                    class="graph-node"
                    class:is-center={node.id === hood.center.id}
                    transform="translate({at.x} {at.y})"
                    role="button"
                    tabindex="0"
                    aria-label={`Focus ${node.name}`}
                    onpointerdown={(event) => onNodePointerDown(event, node.id)}
                    onclick={() => onNodeClick(node.id)}
                    onkeydown={(event) => onNodeKey(event, node.id)}
                  >
                    <circle r={radii[node.id] ?? 6}></circle>
                    <text y={(radii[node.id] ?? 6) + 12}>{nodeLabel(node.name)}</text>
                  </g>
                {/if}
              {/each}
            </g>
          </svg>
          <div class="graph-legend" aria-hidden="true">
            {#each EDGE_KINDS as entry (entry.kind)}
              <span><i class="legend-line legend-{entry.kind}"></i>{entry.label}</span>
            {/each}
          </div>
          {#if hood.truncated}<p class="canvas-note">Showing the closest {formatCount(drawnNodes.length)} symbols.</p>{/if}
        {/if}
      </div>

      <aside class="graph-detail" aria-label="Symbol details">
        {#if !selected}
          <p class="detail-empty">Nothing selected — pick a symbol to see its callers, callees and change impact.</p>
        {:else}
          <div class="detail-head">
            <h2>{selected.name}</h2>
            <span class="kind-pill">{symbolKindLabel(selected.kind)}</span>
            <div class="detail-location">
              <code>{selected.file}:{selected.line}</code>
              <button class="icon-button" aria-label="Copy location" onclick={copyLocation}>
                {#if copied}<Check size={13} />{:else}<Copy size={13} />{/if}
              </button>
            </div>
          </div>
          {#if context}
            <section class="detail-section" aria-label="Callers">
              <h3>Callers <span class="detail-count">{formatCount(context.caller_total)}</span></h3>
              {#if context.callers.length === 0}
                <p class="detail-none">No callers.</p>
              {:else}
                <ul>
                  {#each context.callers as caller (caller.id)}
                    <li>
                      <button class="detail-row" onclick={() => focus(caller)}>
                        <span class="detail-name">{caller.name}</span>
                        <span class="detail-meta">{caller.file}:{caller.line}</span>
                      </button>
                    </li>
                  {/each}
                </ul>
                {#if context.callers.length < context.caller_total}
                  <p class="detail-more">+{formatCount(context.caller_total - context.callers.length)} more</p>
                {/if}
              {/if}
            </section>
            <section class="detail-section" aria-label="Callees">
              <h3>Callees <span class="detail-count">{formatCount(context.callee_total)}</span></h3>
              {#if context.callees.length === 0}
                <p class="detail-none">No callees.</p>
              {:else}
                <ul>
                  {#each context.callees as callee (callee.id)}
                    <li>
                      <button class="detail-row" onclick={() => focus(callee)}>
                        <span class="detail-name">{callee.name}</span>
                        <span class="detail-meta">{callee.file}:{callee.line}</span>
                      </button>
                    </li>
                  {/each}
                </ul>
                {#if context.callees.length < context.callee_total}
                  <p class="detail-more">+{formatCount(context.callee_total - context.callees.length)} more</p>
                {/if}
              {/if}
            </section>
          {/if}
          <section class="detail-section" aria-label="If you change this">
            <h3>If you change this</h3>
            {#if impact.length === 0 && !loadingView && !viewError}
              <p class="detail-none">Nothing depends on this symbol.</p>
            {:else}
              {#each impactGroups as group (group.label)}
                <h4>{group.label} <span class="detail-count">{formatCount(group.rows.length)}</span></h4>
                <ul>
                  {#each group.rows as row (row.id)}
                    <li>
                      <button class="detail-row" onclick={() => focus(row)}>
                        <span class="detail-name">{row.name}</span>
                        <span class="detail-meta">{row.file}:{row.line}</span>
                      </button>
                    </li>
                  {/each}
                </ul>
              {/each}
            {/if}
          </section>
        {/if}
      </aside>
    </div>
  {/if}
</section>

<style>
  .graph-view { display: flex; flex: 1; min-width: 0; min-height: 0; flex-direction: column; background: var(--canvas); }
  .graph-header { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 18px 24px 12px; }
  .graph-heading h1 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 500; letter-spacing: -0.03em; }
  .status-line { margin: 6px 0 0; color: var(--muted); font-size: var(--text-meta); }
  .index-pulse { display: inline-flex; align-items: center; gap: 5px; margin-left: 10px; color: var(--accent-strong); font-weight: 600; }
  .pulse-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--accent-strong); animation: pulse 1.2s ease-in-out infinite; }
  .spin { animation: spin 0.9s linear infinite; }
  .graph-note { display: grid; flex: 1; place-items: center; align-content: center; gap: 10px; padding: 40px 24px; color: var(--muted); text-align: center; }
  .graph-note p { margin: 0; font-size: var(--text-body); max-width: 360px; line-height: 1.5; }

  .graph-body { display: grid; grid-template-columns: 264px minmax(0, 1fr) 288px; flex: 1; min-height: 0; border-top: 1px solid var(--line); }
  .graph-side { display: flex; min-height: 0; flex-direction: column; gap: 10px; padding: 14px; border-right: 1px solid var(--line); overflow-y: auto; }
  .search-box { display: flex; align-items: center; gap: 7px; padding: 0 10px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); color: var(--muted); }
  .search-box:focus-within { border-color: var(--line-strong); }
  .search-box input { flex: 1; min-width: 0; padding: 8px 0; border: 0; background: transparent; color: var(--ink); font-size: var(--text-body); outline: none; }
  .kind-chips { display: flex; flex-wrap: wrap; gap: 4px; }
  .kind-chips button { padding: 3px 8px; border: 1px solid var(--line); border-radius: 999px; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-meta); }
  .kind-chips button:hover { color: var(--ink); }
  .kind-chips button.active { border-color: var(--accent); background: var(--accent-soft); color: var(--accent-strong); font-weight: 600; }
  .side-heading { margin: 4px 2px 0; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.05em; text-transform: uppercase; }
  .side-error { margin: 0; color: var(--danger); font-size: var(--text-meta); }
  .side-empty { margin: 8px 2px; color: var(--muted); font-size: var(--text-body); line-height: 1.5; }
  .symbol-list { display: grid; gap: 2px; margin: 0; padding: 0; list-style: none; }
  .symbol-row { display: grid; gap: 2px; width: 100%; padding: 7px 9px; border: 0; border-radius: 8px; background: transparent; cursor: pointer; text-align: left; }
  .symbol-row:hover { background: var(--surface); box-shadow: inset 0 0 0 1px var(--line); }
  .symbol-row.selected { background: var(--accent-soft); box-shadow: inset 0 0 0 1px var(--accent); }
  .symbol-name { overflow: hidden; color: var(--ink); font-size: var(--text-body); font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
  .symbol-meta { overflow: hidden; color: var(--muted); font-size: var(--text-meta); font-family: var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }

  .graph-canvas-wrap { position: relative; min-width: 0; min-height: 0; overflow: hidden; }
  .canvas-empty { display: grid; height: 100%; place-items: center; align-content: center; gap: 10px; padding: 40px 24px; color: var(--muted); text-align: center; }
  .canvas-empty p { margin: 0; max-width: 320px; font-size: var(--text-body); line-height: 1.5; }
  .canvas-hint { color: var(--muted); font-size: var(--text-meta); }
  .canvas-tools { position: absolute; top: 10px; right: 10px; left: 10px; z-index: 2; display: flex; gap: 6px; }
  .canvas-spacer { flex: 1; }
  .canvas-zoom { width: 26px; height: 26px; border-radius: 7px; background: rgb(255 255 255 / 85%); }
  .tool-chip { padding: 4px 10px; border: 1px solid var(--line); border-radius: 999px; background: rgb(255 255 255 / 85%); color: var(--ink-soft); cursor: pointer; font-size: var(--text-meta); font-weight: 600; }
  .tool-chip[aria-pressed="true"] { border-color: var(--accent); background: var(--accent-soft); color: var(--accent-strong); }
  .graph-canvas { position: absolute; inset: 0; width: 100%; height: 100%; touch-action: none; user-select: none; }
  .graph-canvas .edge { pointer-events: none; }
  .edge-calls { stroke: var(--ink-soft); stroke-width: 1.4; }
  .edge-imports { stroke: var(--muted); stroke-width: 1.1; stroke-dasharray: 5 4; }
  .edge-contains { stroke: var(--accent); stroke-width: 1.1; stroke-dasharray: 2 4; }
  .graph-node { cursor: pointer; }
  .graph-node circle { fill: var(--surface); stroke: var(--line-strong); stroke-width: 1.2; }
  .graph-node:hover circle { stroke: var(--accent-strong); }
  .graph-node:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .graph-node text { fill: var(--ink-soft); font-family: var(--font-ui); font-size: var(--text-meta); text-anchor: middle; }
  .graph-node.is-center circle { fill: var(--accent-soft); stroke: var(--accent-strong); stroke-width: 1.6; }
  .graph-node.is-center text { fill: var(--ink); font-weight: 650; }
  .graph-legend { position: absolute; bottom: 10px; left: 12px; display: flex; gap: 12px; color: var(--muted); font-size: var(--text-meta); }
  .legend-line { display: inline-block; width: 16px; margin-right: 5px; vertical-align: middle; border-top: 2px solid var(--ink-soft); }
  .legend-imports { border-top: 2px dashed var(--muted); }
  .legend-contains { border-top: 2px dotted var(--accent); }
  .canvas-note { position: absolute; right: 12px; bottom: 10px; margin: 0; color: var(--muted); font-size: var(--text-meta); }

  .graph-detail { display: flex; min-height: 0; flex-direction: column; gap: 12px; padding: 14px; border-left: 1px solid var(--line); overflow-y: auto; }
  .detail-empty { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.5; }
  .detail-head h2 { margin: 0; overflow-wrap: anywhere; font-size: var(--text-title); font-weight: 650; }
  .kind-pill { display: inline-block; margin-top: 5px; padding: 2px 7px; border-radius: 999px; background: var(--surface-soft); color: var(--ink-soft); font-size: var(--text-meta); }
  .detail-location { display: flex; align-items: center; gap: 7px; margin-top: 7px; }
  .detail-location code { overflow: hidden; color: var(--muted); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .detail-location .icon-button { flex: 0 0 auto; width: 24px; height: 24px; border-radius: 6px; }
  .detail-section h3 { margin: 0; color: var(--ink-soft); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.05em; text-transform: uppercase; }
  .detail-section h4 { margin: 8px 0 2px; color: var(--ink-soft); font-size: var(--text-meta); font-weight: 600; }
  .detail-count { margin-left: 4px; color: var(--muted); font-weight: 500; }
  .detail-section ul { display: grid; gap: 2px; margin: 4px 0 0; padding: 0; list-style: none; }
  .detail-row { display: grid; gap: 1px; width: 100%; padding: 5px 7px; border: 0; border-radius: 7px; background: transparent; cursor: pointer; text-align: left; }
  .detail-row:hover { background: var(--surface); box-shadow: inset 0 0 0 1px var(--line); }
  .detail-name { overflow: hidden; color: var(--ink); font-size: var(--text-body); text-overflow: ellipsis; white-space: nowrap; }
  .detail-meta { overflow: hidden; color: var(--muted); font-size: var(--text-meta); font-family: var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }
  .detail-more { margin: 4px 0 0; color: var(--muted); font-size: var(--text-meta); }
  .detail-none { margin: 4px 0 0; color: var(--muted); font-size: var(--text-body); }

  @keyframes spin { to { transform: rotate(360deg); } }
  @keyframes pulse { 50% { opacity: 0.25; } }

  @media (max-width: 880px) {
    .graph-body { display: flex; flex-direction: column; overflow-y: auto; }
    .graph-side, .graph-detail { border: 0; overflow: visible; }
    .graph-canvas-wrap { flex: 0 0 auto; height: 320px; border-top: 1px solid var(--line); border-bottom: 1px solid var(--line); order: 2; }
    .graph-side { order: 1; }
    .graph-detail { order: 3; }
  }
</style>