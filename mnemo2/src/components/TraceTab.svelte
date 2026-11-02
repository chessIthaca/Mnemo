<script lang="ts">
  // Copyright (c) 2026 Carsten Hess
  // SPDX-License-Identifier: MIT
  // See LICENSE in the repository root.

  /**
   * The compact Trace tab: the newest model calls, one call's Overview,
   * Context and Response inline, and a hand-over into the full Trace route.
   *
   * It reads the same view model the route does (`./lib/trace` over the frozen
   * `trace_list`/`trace_detail` commands), so the two can never drift: the tab
   * is the route with a 20-row limit, three quick filters and no charts, filter
   * editor, export or destructive actions — those live on the full page.
   */
  import { onMount } from "svelte";
  import { Archive, ArrowLeft, ArrowUpRight, RotateCw } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import {
    actorLabel,
    actorSubLabel,
    cacheReuseLabel,
    callStatus,
    createTracePageTransport,
    formatDuration,
    formatTokens,
    phaseTotalMs,
    previousComparableRow,
    purposeLabel,
    rowTokenSummary,
    traceAge,
    traceClockMs,
    traceCostLabel,
    traceKey,
    type TraceDetail,
    type TraceEntry,
    type TraceListPage,
    type TracePageTransport,
    type TraceRow,
  } from "../lib/trace";
  import TraceContext from "./TraceContext.svelte";
  import TraceOverview from "./TraceOverview.svelte";
  import TraceResponse from "./TraceResponse.svelte";

  /** The tab shows the newest calls; the route pages through the rest. */
  const COMPACT_LIMIT = 20;
  /** The list re-reads itself while the tab is open and the window visible. */
  const POLL_MS = 3_000;
  /** An in-flight call's detail re-reads at the record's own pace. */
  const DETAIL_POLL_MS = 2_000;

  const transport: TracePageTransport | null = createTracePageTransport();

  function errorMessage(value: unknown): string {
    return value instanceof Error ? value.message : String(value);
  }

  /** The worker whose panel or detail is open, if any — the tab's third filter. */
  const activeWorker = $derived.by(() => {
    const id = $app.workerPanelId ?? $app.selectedWorkerId;
    if (id === null) return null;
    return $app.workers.find((worker) => worker.id === id) ?? null;
  });

  /** "all", "lead", or the active worker's id — the query's agent filter. */
  let agent = $state("all");
  let rows = $state<TraceRow[]>([]);
  let total = $state(0);
  let loading = $state(false);
  let loadedOnce = $state(false);
  let listError = $state("");
  let nowMs = $state(Date.now());

  let selectedKey = $state<string | null>(null);
  let detail = $state<TraceDetail | null>(null);
  let detailError = $state("");
  let previous = $state<TraceDetail | null>(null);
  let previousRow = $state<TraceRow | null>(null);
  let previousPending = $state(false);
  let section = $state<"overview" | "context" | "response">("overview");

  let listToken = 0;
  let detailToken = 0;

  const filterOptions = $derived.by((): { value: string; label: string }[] => {
    const options = [
      { value: "all", label: "All agents" },
      { value: "lead", label: "This lead" },
    ];
    if (activeWorker !== null) options.push({ value: activeWorker.id, label: activeWorker.name });
    return options;
  });

  /** The selected row, preferring the live list entry over the fetched detail. */
  const selectedRow = $derived.by((): TraceRow | null => {
    if (selectedKey === null) return null;
    const known = rows.find((row) => traceKey(row) === selectedKey);
    return known ?? detail;
  });

  const scopeNote = $derived.by(() => {
    const actor =
      agent === "all" ? "the lead and every worker" : agent === "lead" ? "the lead" : activeWorker?.name ?? "this worker";
    const kept = loadedOnce ? (total > 0 ? ` The log holds ${total} matching ${total === 1 ? "call" : "calls"}.` : "") : "";
    return `The latest ${COMPACT_LIMIT} model calls from ${actor}, newest first.${kept}`;
  });

  function query(): { agent?: string; limit: number } {
    return { agent: agent === "all" ? undefined : agent, limit: COMPACT_LIMIT };
  }

  async function fetchPage(): Promise<TraceListPage | null> {
    if (transport === null) return null;
    return transport.list(query());
  }

  async function load(): Promise<void> {
    if (transport === null) return;
    const token = ++listToken;
    loading = true;
    try {
      const page = await fetchPage();
      if (page === null || token !== listToken) return;
      rows = page.rows;
      total = page.total;
      listError = "";
      loadedOnce = true;
      nowMs = Date.now();
    } catch (failure) {
      if (token !== listToken) return;
      listError = errorMessage(failure);
    } finally {
      if (token === listToken) loading = false;
    }
  }

  /** A quiet refresh: the tab keeps the last good list when a poll fails. */
  async function poll(): Promise<void> {
    if (transport === null || loading || document.visibilityState === "hidden") return;
    const token = listToken;
    try {
      const page = await fetchPage();
      if (page === null || token !== listToken) return;
      rows = page.rows;
      total = page.total;
      nowMs = Date.now();
    } catch {
      // The next tick tries again.
    }
  }

  async function loadDetail(key: string): Promise<void> {
    if (transport === null) return;
    const split = key.lastIndexOf(":");
    if (split <= 0) return;
    const runId = key.slice(0, split);
    const id = Number(key.slice(split + 1));
    const token = ++detailToken;
    detailError = "";
    try {
      const loaded = await transport.detail(runId, id);
      if (token !== detailToken) return;
      if (loaded === null) {
        detail = null;
        detailError = "The full record for this call is no longer available in the live log.";
      } else {
        detail = loaded;
        void loadPrevious();
      }
    } catch (failure) {
      if (token !== detailToken) return;
      detailError = errorMessage(failure);
    }
  }

  /** The previous comparable call, so Context can show the prompt-prefix reuse. */
  async function loadPrevious(): Promise<void> {
    if (transport === null || selectedRow === null) return;
    const row = previousComparableRow(rows, selectedRow);
    if (row === null) {
      previous = null;
      previousRow = null;
      previousPending = false;
      return;
    }
    previousRow = row;
    previousPending = true;
    try {
      previous = await transport.detail(row.run_id, row.id);
    } catch {
      previous = null;
    } finally {
      previousPending = false;
    }
  }

  /** Refresh one still-streaming call's detail when the list says it moved. */
  async function pollDetail(): Promise<void> {
    if (transport === null || selectedKey === null || detail === null) return;
    if (document.visibilityState === "hidden" || selectedRow === null) return;
    if (selectedRow.is_complete || selectedRow.version === detail.version) return;
    await loadDetail(selectedKey);
  }

  function selectRow(row: TraceRow): void {
    selectedKey = traceKey(row);
    detail = null;
    detailError = "";
    previous = null;
    previousRow = null;
    section = "overview";
    void loadDetail(selectedKey);
  }

  function closeDetail(): void {
    selectedKey = null;
    detail = null;
    detailError = "";
    previous = null;
    previousRow = null;
    // Drop a detail reply that is still in flight.
    detailToken += 1;
  }

  function setAgent(value: string): void {
    if (agent === value) return;
    agent = value;
    closeDetail();
    rows = [];
    total = 0;
    loadedOnce = false;
    listError = "";
    void load();
  }

  /** Carry the filter and the open call into the full route. */
  function openFullTrace(): void {
    const entry: TraceEntry = {};
    if (agent !== "all") entry.agent = agent;
    if (selectedKey !== null) entry.selected = selectedKey;
    if (Object.keys(entry).length === 0) actions.openTrace();
    else actions.openTrace(entry);
  }

  /** The row's cost: dollars when priced, an em dash when the model has no price. */
  function costLabel(row: TraceRow): string {
    return traceCostLabel(row.cost_micros);
  }

  onMount(() => {
    if (transport === null) return;
    void load();
    const listTimer = setInterval(() => void poll(), POLL_MS);
    const detailTimer = setInterval(() => void pollDetail(), DETAIL_POLL_MS);
    return () => {
      clearInterval(listTimer);
      clearInterval(detailTimer);
    };
  });
</script>

<section class="trace-tab" aria-label="Model call trace">
  <header class="tab-head">
    <div class="tab-title">
      <h2>Trace</h2>
      <p class="tab-scope">{scopeNote}</p>
    </div>
    <div class="tab-actions">
      <button type="button" class="button button-quiet" onclick={() => void load()} disabled={!transport || loading}>
        <RotateCw size={12} /> Refresh
      </button>
      <button type="button" class="button" aria-label="Open the full Trace page" onclick={openFullTrace}>
        <ArrowUpRight size={12} /> Open full Trace
      </button>
    </div>
  </header>

  {#if !transport}
    <p class="tab-note">The Trace tab needs the desktop app.</p>
  {:else if selectedKey === null}
    <div class="filters" role="group" aria-label="Filter calls by agent">
      {#each filterOptions as option (option.value)}
        <button
          type="button"
          class="filter"
          aria-pressed={agent === option.value}
          class:active={agent === option.value}
          onclick={() => setAgent(option.value)}
        >{option.label}</button>
      {/each}
    </div>

    {#if listError}
      <div class="state-card" role="alert">
        <p>Could not read the model calls: {listError}</p>
        <button type="button" class="button button-quiet" onclick={() => void load()}>Try again</button>
      </div>
    {:else if !loadedOnce && loading}
      <p class="tab-note">Loading model calls…</p>
    {:else if rows.length === 0}
      <div class="state-card">
        <p>{agent === "all" ? "No model calls yet." : "No model calls from this agent yet."}</p>
        <p class="state-hint">
          The lead's turns and each worker's calls appear here the moment they start.
          {#if agent !== "all"}<button type="button" class="link-button" onclick={() => setAgent("all")}>Show all agents</button>{/if}
        </p>
      </div>
    {:else}
      <ul class="call-list">
        {#each rows as row (traceKey(row))}
          {@const status = callStatus(row)}
          {@const tokens = rowTokenSummary(row)}
          <li>
            <button
              type="button"
              class="call"
              aria-label={`Open call ${row.id}: ${actorLabel(row.attribution)}, ${status.label}`}
              onclick={() => selectRow(row)}
            >
              <span class="call-head">
                <span class="status-dot {status.tone}" aria-hidden="true"></span>
                <span class="who">{actorLabel(row.attribution)}</span>
                <span class="what">{purposeLabel(row.attribution?.purpose ?? "other")}</span>
                <span class="spacer"></span>
                {#if row.retained}
                  <Archive size={12} class="warn" title="Retained metadata — the prompt and response were not kept" aria-label="Retained metadata" />
                {/if}
                <span class="status {status.tone}">{status.label}</span>
              </span>
              {#if actorSubLabel(row.attribution)}
                <span class="sub">{actorSubLabel(row.attribution)}</span>
              {/if}
              <span class="numbers">
                <span title="Model">{row.model}</span>
                <span title="Prompt-cache reuse">{cacheReuseLabel(row.usage)}</span>
                <span title="Tokens in · out">{tokens.input === null ? "—" : formatTokens(tokens.input)} in · {tokens.output === null ? "—" : formatTokens(tokens.output)} out</span>
                <span title="Estimated cost" class:pending={row.cost_micros === null}>{costLabel(row)}</span>
              </span>
              <span class="when" title={traceAge(row.ts_ms, nowMs)}>
                {traceClockMs(row.ts_ms)} · {formatDuration(phaseTotalMs(row))}
              </span>
            </button>
          </li>
        {/each}
      </ul>
      {#if total > rows.length}
        <p class="more-note">
          Showing {rows.length} of {total}. <button type="button" class="link-button" onclick={openFullTrace}>Open the full Trace page</button> for older calls, filters and charts.
        </p>
      {/if}
    {/if}
  {:else}
    <div class="detail">
      <header class="detail-head">
        <button type="button" class="back" aria-label="Back to the call list" onclick={closeDetail}>
          <ArrowLeft size={13} /> Back
        </button>
        {#if selectedRow !== null}
          {@const status = callStatus(selectedRow)}
          <div class="detail-title">
            <p class="detail-line">
              <span class="status-dot {status.tone}" aria-hidden="true"></span>
              <strong>{actorLabel(selectedRow.attribution)}</strong>
              <span class="muted">{purposeLabel(selectedRow.attribution?.purpose ?? "other")}</span>
            </p>
            <p class="detail-line muted">#{selectedRow.id} · {traceClockMs(selectedRow.ts_ms)}</p>
          </div>
          <span class="status {status.tone}">{status.label}</span>
        {/if}
      </header>

      {#if detailError}
        <p class="tab-note" role="alert">{detailError}</p>
      {:else if detail === null}
        <p class="tab-note">Loading the call…</p>
      {:else if selectedRow !== null}
        <div class="sections" role="group" aria-label="Call sections">
          {#each [{ value: "overview", label: "Overview" }, { value: "context", label: "Context" }, { value: "response", label: "Response" }] as entry (entry.value)}
            <button
              type="button"
              class="section"
              aria-pressed={section === entry.value}
              class:active={section === entry.value}
              onclick={() => (section = entry.value as "overview" | "context" | "response")}
            >{entry.label}</button>
          {/each}
        </div>
        <div class="detail-body">
          {#if section === "overview"}
            <TraceOverview row={selectedRow} {detail} {nowMs} />
          {:else if section === "context"}
            <TraceContext {detail} {previous} {previousRow} {previousPending} />
          {:else}
            <TraceResponse row={selectedRow} {detail} />
          {/if}
        </div>
      {/if}
      <footer class="detail-foot">
        <button type="button" class="link-button" onclick={openFullTrace}>Open this call in the full Trace page</button>
      </footer>
    </div>
  {/if}
</section>

<style>
  .trace-tab { display: grid; grid-template-rows: auto auto 1fr; gap: 10px; min-height: 0; height: 100%; padding: 14px 14px 12px; }
  .tab-head { display: flex; align-items: flex-start; gap: 10px; }
  .tab-title { flex: 1; display: grid; gap: 3px; min-width: 0; }
  .tab-title h2 { margin: 0; color: var(--ink); font-size: var(--text-title); font-weight: 650; }
  .tab-scope { margin: 0; color: var(--muted); font-size: var(--text-meta); line-height: 1.45; }
  .tab-actions { display: flex; gap: 6px; flex-wrap: wrap; justify-content: flex-end; }
  .button { display: inline-flex; align-items: center; gap: 5px; padding: 5px 9px; border: 1px solid var(--line-strong); border-radius: 7px; background: var(--surface); color: var(--ink); cursor: pointer; font-size: var(--text-meta); font-weight: 600; }
  .button:hover:not(:disabled) { border-color: var(--accent-line); color: var(--accent-strong); }
  .button:disabled { color: var(--disabled-ink); cursor: default; }
  .button-quiet { background: transparent; border-color: var(--line); color: var(--ink-soft); }
  .filters { display: flex; gap: 4px; flex-wrap: wrap; }
  .filter { padding: 4px 10px; border: 1px solid var(--line); border-radius: 999px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: var(--text-meta); font-weight: 600; }
  .filter:hover { border-color: var(--accent-line); color: var(--accent-strong); }
  .filter.active { border-color: var(--accent-line); background: var(--accent-soft); color: var(--accent-strong); }
  .filter:focus-visible, .button:focus-visible, .call:focus-visible, .section:focus-visible { outline: 2px solid var(--focus-line); outline-offset: 1px; }
  .call-list { display: grid; gap: 6px; margin: 0; padding: 0 2px 0 0; list-style: none; overflow: auto; min-height: 0; }
  .call { display: grid; gap: 3px; width: 100%; padding: 9px 11px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); cursor: pointer; text-align: left; }
  .call:hover { border-color: var(--accent-line); background: var(--surface-soft); }
  .call-head { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .who { color: var(--ink); font-size: var(--text-body); font-weight: 650; }
  .what { color: var(--muted); font-size: var(--text-meta); }
  .spacer { flex: 1; }
  .sub { color: var(--muted); font-size: var(--text-meta); }
  .numbers { display: flex; gap: 8px; flex-wrap: wrap; color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-meta); font-variant-numeric: tabular-nums; }
  .numbers .pending { color: var(--muted); }
  .when { color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); font-variant-numeric: tabular-nums; }
  .status-dot { flex: 0 0 auto; width: 7px; height: 7px; border-radius: 999px; background: var(--muted); }
  .status-dot.ok { background: var(--stat-add); }
  .status-dot.live { background: var(--running-ink); }
  .status-dot.error { background: var(--danger); }
  .status-dot.stopped { background: var(--muted); }
  .status-dot.guard { background: var(--warning); }
  .status { flex: 0 0 auto; padding: 2px 8px; border-radius: 999px; background: var(--surface-soft); color: var(--muted); font-size: var(--text-meta); font-weight: 650; }
  .status.ok { background: var(--stat-add-bg); color: var(--stat-add); }
  .status.live { background: var(--running-bg); color: var(--running-ink); }
  .status.error { background: var(--danger-soft); color: var(--danger); }
  .status.stopped { background: var(--surface-soft); color: var(--muted); }
  .status.guard { background: var(--warning-soft); color: var(--warning); }
  .warn { color: var(--warning); }
  .tab-note { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.5; }
  .state-card { display: grid; gap: 6px; padding: 14px; border: 1px solid var(--line); border-radius: 11px; background: var(--surface-soft); color: var(--ink-soft); font-size: var(--text-body); }
  .state-card p { margin: 0; }
  .state-hint { color: var(--muted); line-height: 1.5; }
  .link-button { padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: inherit; font-weight: 600; }
  .link-button:hover { text-decoration: underline; }
  .more-note { margin: 0; color: var(--muted); font-size: var(--text-meta); line-height: 1.5; }
  .detail { display: grid; grid-template-rows: auto auto auto 1fr auto; gap: 8px; min-height: 0; }
  .detail-head { display: flex; align-items: flex-start; gap: 8px; }
  .back { display: inline-flex; align-items: center; gap: 4px; padding: 4px 8px; border: 1px solid var(--line); border-radius: 7px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: var(--text-meta); font-weight: 600; }
  .back:hover { border-color: var(--accent-line); color: var(--accent-strong); }
  .detail-title { flex: 1; display: grid; gap: 2px; min-width: 0; }
  .detail-line { display: flex; align-items: center; gap: 6px; margin: 0; color: var(--ink); font-size: var(--text-body); line-height: 1.4; }
  .detail-line.muted { color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); }
  .sections { display: flex; gap: 2px; border-bottom: 1px solid var(--line); }
  .section { padding: 6px 10px; border: 0; border-bottom: 2px solid transparent; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-body); font-weight: 600; }
  .section:hover { color: var(--ink-soft); }
  .section.active { border-bottom-color: var(--accent); color: var(--ink); }
  .detail-body { overflow: auto; min-height: 0; }
  .detail-foot { border-top: 1px solid var(--line); padding-top: 7px; }
</style>
