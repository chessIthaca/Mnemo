<script lang="ts">
  // Copyright (c) 2026 Carsten Hess
  // SPDX-License-Identifier: MIT
  // See LICENSE in the repository root.

  import { onMount } from "svelte";
  import { Copy, Download, FolderOpen, History, Search, Trash2, X } from "@lucide/svelte";
  import { formatBytes } from "../lib/files";
  import { actions, app, SESSION_START_MS } from "../lib/store";
  import TraceCharts from "./TraceCharts.svelte";
  import TraceDetailPane from "./TraceDetail.svelte";
  import TraceList from "./TraceList.svelte";
  import {
    activeTraceChips,
    createTracePageTransport,
    DEFAULT_TRACE_FILTERS,
    formatTokens,
    groupTraceRows,
    loadTracePrefs,
    previousComparableRow,
    saveTracePrefs,
    traceAgentOptions,
    traceCostLabel,
    traceEntryFilters,
    traceKey,
    traceListQuery,
    traceModelOptions,
    TRACE_PAGE_SIZE,
    TRACE_PURPOSE_OPTIONS,
    TRACE_STATUS_OPTIONS,
    TRACE_TIME_OPTIONS,
    type TraceDetail,
    type TraceExportRequest,
    type TraceExportResult,
    type TraceFilters,
    type TraceListPage,
    type TraceListStatus,
    type TraceRow,
    type TraceSearchScope,
    type TraceSettings,
    type TraceGrouping,
  } from "../lib/trace";

  /** How often an open page re-reads the filtered list. */
  const POLL_MS = 1_500;
  /** How often an in-flight selected call re-reads its detail. */
  const DETAIL_POLL_MS = 1_000;
  /** The search input's debounce. */
  const SEARCH_DEBOUNCE_MS = 300;
  /** Below this width of its own the view shows the list or one call, not both. */
  const NARROW_VIEW_WIDTH = 760;
  const LIST_WIDTH_MIN = 300;
  const LIST_WIDTH_MAX = 720;
  const INSIGHTS_HEIGHT_MIN = 160;
  const INSIGHTS_HEIGHT_MAX = 900;

  function errorMessage(value: unknown): string {
    return value instanceof Error ? value.message : String(value);
  }

  const transport = createTracePageTransport();

  let filters = $state<TraceFilters>({ ...DEFAULT_TRACE_FILTERS });
  let rows = $state<TraceRow[]>([]);
  let observed = $state<TraceRow[]>([]);
  let total = $state(0);
  let unpriced = $state(0);
  let hasMore = $state(false);
  let nextCursor = $state<string | null>(null);
  let capped = $state(false);
  let listError = $state("");
  let loadingList = $state(false);
  let loadingMore = $state(false);
  let loadedOnce = $state(false);

  let follow = $state(true);
  let newCount = $state(0);
  let nowMs = $state(Date.now());

  let selectedKey = $state<string | null>(null);
  let detail = $state<TraceDetail | null>(null);
  let detailError = $state("");
  let detailLoading = $state(false);
  let previousDetail = $state<TraceDetail | null>(null);
  let previousRow = $state<TraceRow | null>(null);
  let previousPending = $state(false);

  // Open by default: the Lead's per-call evolution is the tab's headline.
  let insightsOpen = $state(true);
  let listWidth = $state(420);
  let narrow = $state(false);
  let dragging = $state(false);
  let columns: HTMLElement | undefined = $state();

  // The Insights charts' body height in px; null keeps the natural layout.
  let insightsHeight = $state<number | null>(null);
  let insightsDragging = $state(false);
  let insightsBody: HTMLElement | undefined = $state();
  let traceRoot: HTMLElement | undefined = $state();
  let plotScale = $state(1);
  // Plain mirror of the scale currently applied to the plots, for measuring.
  let appliedScale = 1;

  let reloadTimer: ReturnType<typeof setTimeout> | null = null;
  let reloadToken = 0;
  let detailToken = 0;

  // --- Data controls: export, on-disk bodies, clear and delete --------------
  /** Which export scope is running, or `null` when idle. */
  type ExportScope = TraceExportRequest["scope"];
  let settings = $state<TraceSettings | null>(null);
  let settingsError = $state("");
  let settingsBusy = $state(false);
  /** The durable keys ticked for a "selected calls" export. */
  let checked = $state<string[]>([]);
  let exporting = $state<ExportScope | null>(null);
  let exportResult = $state<TraceExportResult | null>(null);
  let exportError = $state("");
  let exportNote = $state("");
  let copiedPath = $state(false);
  let confirmClear = $state(false);
  let confirmDelete = $state(false);
  let maintenanceBusy = $state(false);
  let maintenanceNote = $state("");
  let maintenanceError = $state("");

  const liveSummary = $derived.by(() => {
    if (settings === null) return "";
    const parts = [`${settings.ring_capacity} calls kept in memory`];
    parts.push(settings.log_bodies ? "bodies kept for the live log" : "bodies not kept for the live log");
    return parts.join(" · ");
  });

  const journalSummary = $derived.by(() => {
    if (settings === null) return "";
    return `Retained metadata ${formatBytes(settings.history_bytes)} · journal ${formatBytes(settings.journal_bytes)}`;
  });

  /** Only the ticks that are still listed — a filter change silently drops the rest. */
  const pickedKeys = $derived(checked.filter((key) => rows.some((row) => traceKey(row) === key)));

  /** How many listed calls are metadata-only, for the delete confirmation. */
  const retainedInView = $derived(rows.filter((row) => row.retained).length);

  const chips = $derived(activeTraceChips(filters));
  const groups = $derived(groupTraceRows(rows, filters.grouping, nowMs));
  const agentOptions = $derived(traceAgentOptions(observed, $app.workers));
  const modelOptions = $derived(traceModelOptions(observed));
  const selectedRow = $derived(
    selectedKey === null ? null : rows.find((row) => traceKey(row) === selectedKey) ?? detail,
  );
  const summary = $derived(summaryLine());
  const statusOptions: { value: "all" | TraceListStatus; label: string }[] = [
    { value: "all", label: "All statuses" },
    ...TRACE_STATUS_OPTIONS,
  ];
  const scopeOptions: { value: TraceSearchScope; label: string }[] = [
    { value: "all", label: "All content" },
    { value: "request", label: "Request" },
    { value: "response", label: "Response" },
    { value: "reasoning", label: "Reasoning" },
    { value: "tools", label: "Tool calls" },
    { value: "metadata", label: "Metadata" },
  ];
  const groupingOptions: { value: TraceGrouping; label: string }[] = [
    { value: "time", label: "Time" },
    { value: "agent", label: "Agent" },
    { value: "turn", label: "Turn" },
  ];

  /** The wireframe's summary line: calls, tokens, cost. */
  function summaryLine(): string {
    let input = 0;
    let output = 0;
    let costMicros: number | null = null;
    for (const row of rows) {
      if (row.usage !== null) {
        input += row.usage.prompt;
        output += row.usage.completion;
      }
      if (row.cost_micros !== null) costMicros = (costMicros ?? 0) + row.cost_micros;
    }
    const tokens = input + output;
    const cost = costMicros === null ? "—" : `${traceCostLabel(costMicros)}${unpriced > 0 ? "+" : ""}`;
    return `${total.toLocaleString()} ${total === 1 ? "call" : "calls"} · ${formatTokens(tokens)} tokens · ${cost}`;
  }

  function savePrefs(): void {
    saveTracePrefs($app.projectId, { ...filters, insightsOpen, listWidth, insightsHeight });
  }

  /** Coalesce rapid filter changes into one reload. */
  function scheduleReload(delay: number): void {
    if (reloadTimer !== null) clearTimeout(reloadTimer);
    reloadTimer = setTimeout(() => {
      reloadTimer = null;
      void reload();
    }, delay);
  }

  /** Update one filter, persist it, and refetch. */
  function setFilter(key: "agent" | "model" | "status" | "purpose", value: string): void {
    filters = { ...filters, [key]: value } as TraceFilters;
    savePrefs();
    scheduleReload(0);
  }

  /** The search box refetches after a quiet spell, not per keystroke. */
  function setSearch(value: string): void {
    filters = { ...filters, search: value };
    savePrefs();
    scheduleReload(SEARCH_DEBOUNCE_MS);
  }

  function setTime(value: TraceFilters["time"]): void {
    filters = { ...filters, time: value, from_ms: filters.from_ms, to_ms: filters.to_ms };
    savePrefs();
    scheduleReload(0);
  }

  function setCustomFrom(value: string): void {
    filters = { ...filters, from_ms: value === "" ? null : new Date(value).getTime() };
    savePrefs();
    scheduleReload(0);
  }

  function setCustomTo(value: string): void {
    filters = { ...filters, to_ms: value === "" ? null : new Date(value).getTime() };
    savePrefs();
    scheduleReload(0);
  }

  function setGrouping(value: TraceGrouping): void {
    filters = { ...filters, grouping: value };
    savePrefs();
  }

  function clearChip(key: string): void {
    const reset: Partial<TraceFilters> = {};
    // Removing the agent chip returns to the Lead default — the neutral scope.
    if (key === "agent") reset.agent = "lead";
    else if (key === "model") reset.model = "all";
    else if (key === "status") reset.status = "all";
    else if (key === "purpose") reset.purpose = "all";
    else if (key === "time") {
      reset.time = "session";
      reset.from_ms = null;
      reset.to_ms = null;
    } else if (key === "search") reset.search = "";
    else if (key === "turn_id") reset.turn_id = null;
    filters = { ...filters, ...reset };
    savePrefs();
    scheduleReload(0);
  }

  function clearFilters(): void {
    filters = { ...DEFAULT_TRACE_FILTERS, grouping: filters.grouping, selected: null };
    savePrefs();
    scheduleReload(0);
  }  /** Remember rows for the Agent/Model option lists; options never shrink mid-session. */
  function rememberRows(pageRows: TraceRow[]): void {
    if (pageRows.length === 0) return;
    const seen = new Set(observed.map(traceKey));
    const additions = pageRows.filter((row) => !seen.has(traceKey(row)));
    if (additions.length === 0) return;
    observed = [...observed, ...additions];
  }

  /** One page of the filtered log, replacing whatever was loaded. */
  async function reload(): Promise<void> {
    if (transport === null) return;
    const token = ++reloadToken;
    loadingList = true;
    try {
      const page = await transport.list(
        traceListQuery(filters, { sessionStartMs: SESSION_START_MS, nowMs: Date.now(), limit: TRACE_PAGE_SIZE }),
      );
      if (token !== reloadToken) return;
      listError = "";
      rows = page.rows;
      total = page.total;
      unpriced = page.unpriced;
      hasMore = page.has_more;
      nextCursor = page.next_cursor;
      capped = page.capped;
      newCount = 0;
      nowMs = Date.now();
      loadedOnce = true;
      rememberRows(page.rows);
      restoreSelection();
    } catch (failure) {
      if (token !== reloadToken) return;
      listError = errorMessage(failure);
    } finally {
      if (token === reloadToken) loadingList = false;
    }
  }

  /** Re-open the persisted selection once the first page has arrived. */
  function restoreSelection(): void {
    if (selectedKey !== null || filters.selected === null) return;
    selectedKey = filters.selected;
    void loadDetail(selectedKey);
  }

  /** Merge one polled page: new calls prepend while following, in-flight rows update in place. */
  function mergePolledPage(page: TraceListPage): void {
    nowMs = Date.now();
    total = page.total;
    unpriced = page.unpriced;
    capped = page.capped || capped;
    hasMore = page.has_more || hasMore;
    rememberRows(page.rows);
    const known = new Set(rows.map(traceKey));
    const fresh: TraceRow[] = [];
    const updated = new Map<string, TraceRow>();
    for (const row of page.rows) {
      const key = traceKey(row);
      if (known.has(key)) updated.set(key, row);
      else fresh.push(row);
    }
    if (updated.size === 0 && fresh.length === 0) return;
    let next = rows.map((row) => updated.get(traceKey(row)) ?? row);
    if (follow) next = [...fresh, ...next];
    else newCount += fresh.length;
    rows = next;
  }

  /**
   * The poll: one fresh first page every 1.5s while the tab is visible.
   * A failed poll keeps the last good data and stays quiet — the retry
   * banner is for explicit loads.
   */
  async function poll(): Promise<void> {
    if (transport === null || loadingList || loadingMore) return;
    if (document.visibilityState === "hidden") return;
    // The token drops a poll that raced a filter change mid-flight: its
    // rows would answer the old filters.
    const token = reloadToken;
    try {
      const page = await transport.list(
        traceListQuery(filters, { sessionStartMs: SESSION_START_MS, nowMs: Date.now(), limit: TRACE_PAGE_SIZE }),
      );
      if (token !== reloadToken) return;
      mergePolledPage(page);
    } catch {
      // The next tick tries again.
    }
  }

  /** Append the next page when the user asks for older calls. */
  async function loadMore(): Promise<void> {
    if (transport === null || nextCursor === null) return;
    loadingMore = true;
    try {
      const page = await transport.list(
        traceListQuery(filters, {
          sessionStartMs: SESSION_START_MS,
          nowMs: Date.now(),
          cursor: nextCursor,
          limit: TRACE_PAGE_SIZE,
        }),
      );
      rememberRows(page.rows);
      const known = new Set(rows.map(traceKey));
      rows = [...rows, ...page.rows.filter((row) => !known.has(traceKey(row)))];
      hasMore = page.has_more;
      nextCursor = page.next_cursor;
      total = page.total;
      unpriced = page.unpriced;
      capped = page.capped || capped;
      nowMs = Date.now();
    } catch (failure) {
      listError = errorMessage(failure);
    } finally {
      loadingMore = false;
    }
  }

  /** Open one call's detail by its durable key. `keep` leaves the current
   *  detail on screen while a refresh is in flight, so live polls never
   *  flash the pane empty. */
  async function loadDetail(key: string, keep = false): Promise<void> {
    if (transport === null) return;
    const split = key.lastIndexOf(":");
    const runId = key.slice(0, split);
    const id = Number(key.slice(split + 1));
    const token = ++detailToken;
    detailLoading = true;
    detailError = "";
    if (!keep) {
      detail = null;
      previousDetail = null;
      previousRow = null;
      previousPending = false;
    }
    try {
      const loaded = await transport.detail(runId, id);
      if (token !== detailToken) return;
      if (loaded === null) {
        detailError = "The full record for this call is no longer available in the live log.";
      } else {
        detail = loaded;
        void loadPrevious();
      }
    } catch (failure) {
      if (token !== detailToken) return;
      detailError = errorMessage(failure);
    } finally {
      if (token === detailToken) detailLoading = false;
    }
  }

  /** Fetch the previous comparable call for the Context tab's prefix comparison. */
  async function loadPrevious(): Promise<void> {
    if (transport === null || selectedRow === null) return;
    const previous = previousComparableRow(rows, selectedRow);
    if (previous === null) return;
    // The comparable call is terminal in practice; skip refetching a body
    // the live poll has not changed under the same key.
    if (previousDetail !== null && previousRow !== null && previousRow.run_id === previous.run_id && previousRow.version === previous.version && previousDetail.version === previous.version) {
      return;
    }
    previousRow = previous;
    previousPending = true;
    try {
      previousDetail = await transport.detail(previous.run_id, previous.id);
    } catch {
      previousDetail = null;
    } finally {
      previousPending = false;
    }
  }

  /** The in-flight detail poll: 1s, and only when the row says it changed. */
  async function pollDetail(): Promise<void> {
    if (transport === null || selectedRow === null || detail === null) return;
    if (document.visibilityState === "hidden") return;
    if (selectedRow.is_complete || detailLoading) return;
    if (selectedRow.version === detail.version) return;
    if (selectedKey === null) return;
    await loadDetail(selectedKey, true);
  }

  function selectRow(row: TraceRow): void {
    const key = traceKey(row);
    selectedKey = key;
    // Following only while the selection stays at the head of the list.
    follow = rows.length > 0 && traceKey(rows[0]) === key;
    filters = { ...filters, selected: key };
    savePrefs();
    void loadDetail(key);
  }

  /**
   * Open a call another surface handed over. The row may sit outside the
   * current filters, so the detail loads by key instead of waiting for a row.
   */
  function openEntrySelection(key: string | null): void {
    if (key === null) return;
    selectedKey = key;
    void loadDetail(key);
  }

  function closeDetail(): void {
    selectedKey = null;
    detail = null;
    detailError = "";
    filters = { ...filters, selected: null };
    savePrefs();
  }

  function toggleFollow(on: boolean): void {
    follow = on;
    if (on) {
      newCount = 0;
      void reload();
    }
  }

  function revealNewCalls(): void {
    toggleFollow(true);
  }

  // -------------------------------------------------------------------------
  // Data controls — every call goes through the frozen `trace_*` commands.
  // -------------------------------------------------------------------------

  async function loadSettings(): Promise<void> {
    if (transport === null) return;
    settingsBusy = true;
    try {
      settings = await transport.settings();
      settingsError = "";
    } catch (failure) {
      settingsError = errorMessage(failure);
    } finally {
      settingsBusy = false;
    }
  }

  /** Flip the sensitive "save bodies to disk" switch and report what landed. */
  async function toggleLogBodies(): Promise<void> {
    if (transport === null || settings === null) return;
    settingsBusy = true;
    try {
      settings = await transport.setLogBodies(!settings.log_bodies);
      settingsError = "";
    } catch (failure) {
      settingsError = errorMessage(failure);
    } finally {
      settingsBusy = false;
    }
  }

  function toggleCheck(row: TraceRow): void {
    const key = traceKey(row);
    checked = checked.includes(key) ? checked.filter((entry) => entry !== key) : [...checked, key];
  }

  /** Which keys an export scope would write — empty means the button is inert. */
  function exportKeys(scope: ExportScope): string[] {
    if (scope === "call") return selectedKey === null ? [] : [selectedKey];
    if (scope === "selected") return pickedKeys;
    return [];
  }

  async function exportCalls(scope: ExportScope): Promise<void> {
    if (transport === null || exporting !== null) return;
    const keys = exportKeys(scope);
    if (scope !== "filtered" && keys.length === 0) return;
    exporting = scope;
    exportError = "";
    exportNote = "";
    copiedPath = false;
    try {
      const request: TraceExportRequest = { scope };
      if (scope !== "filtered") request.keys = [...keys];
      else
        request.query = traceListQuery(filters, {
          sessionStartMs: SESSION_START_MS,
          nowMs: Date.now(),
          limit: TRACE_PAGE_SIZE,
        });
      exportResult = await transport.exportCalls(request);
    } catch (failure) {
      exportError = errorMessage(failure);
    } finally {
      exporting = null;
    }
  }

  async function copyExportPath(): Promise<void> {
    if (exportResult === null) return;
    const ok = await copyText(exportResult.path);
    copiedPath = ok;
    exportNote = ok ? "Path copied." : "";
    if (!ok) exportError = "The clipboard is not available — select the path and copy it manually.";
  }

  /** Best-effort clipboard write; the page never depends on it. */
  async function copyText(text: string): Promise<boolean> {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      return false;
    }
  }

  async function revealExport(): Promise<void> {
    if (transport === null || exportResult === null) return;
    try {
      await transport.revealExport(exportResult.path);
      exportError = "";
      exportNote = "Opened the export folder.";
    } catch (failure) {
      exportError = `Could not open the export folder: ${errorMessage(failure)}`;
    }
  }

  async function clearLive(): Promise<void> {
    if (transport === null || maintenanceBusy) return;
    maintenanceBusy = true;
    maintenanceError = "";
    maintenanceNote = "";
    try {
      await transport.clearLive();
      checked = [];
      maintenanceNote = "Cleared the live call log.";
      await refreshAfterMaintenance();
    } catch (failure) {
      maintenanceError = `Could not clear the live call log: ${errorMessage(failure)}`;
    } finally {
      confirmClear = false;
      maintenanceBusy = false;
    }
  }

  async function deleteHistory(): Promise<void> {
    if (transport === null || maintenanceBusy) return;
    maintenanceBusy = true;
    maintenanceError = "";
    maintenanceNote = "";
    try {
      const outcome = await transport.deleteHistory();
      maintenanceNote =
        outcome.deleted_files === 0
          ? "There was no retained history to delete."
          : `Deleted ${outcome.deleted_files} retained file${outcome.deleted_files === 1 ? "" : "s"} (${formatBytes(outcome.bytes)}).`;
      checked = [];
      await refreshAfterMaintenance();
    } catch (failure) {
      maintenanceError = `Could not delete the retained history: ${errorMessage(failure)}`;
    } finally {
      confirmDelete = false;
      maintenanceBusy = false;
    }
  }

  /** Re-read everything a clear or delete changed: list, counts, settings, detail. */
  async function refreshAfterMaintenance(): Promise<void> {
    closeDetail();
    loadedOnce = false;
    await Promise.all([reload(), loadSettings()]);
  }

  function toggleInsights(): void {
    insightsOpen = !insightsOpen;
    savePrefs();
  }

  /** What the Insights charts cover: the active agent and time filter. */
  const insightsScopeLabel = $derived.by(() => {
    const parts: string[] = [];
    if (filters.agent !== "all") {
      const known = agentOptions.find((option) => option.value === filters.agent)?.label ?? filters.agent;
      parts.push(known.toLowerCase());
    }
    const timeLabel = TRACE_TIME_OPTIONS.find((option) => option.value === filters.time)?.label ?? "custom range";
    parts.push(timeLabel.toLowerCase());
    return parts.join(" · ");
  });

  /** Turn a cost-chart bucket into the page's time filter (half-open range). */
  function applyBucket(fromMs: number, toMs: number): void {
    filters = { ...filters, time: "custom", from_ms: fromMs, to_ms: toMs };
    savePrefs();
    scheduleReload(0);
  }

  // The list's drag divider: pointer drag, arrow keys, saved width.
  function onDividerPointerDown(event: PointerEvent) {
    dragging = true;
    const target = event.currentTarget as HTMLElement | null;
    target?.setPointerCapture(event.pointerId);
  }

  function onDividerPointerMove(event: PointerEvent) {
    if (!dragging || columns === undefined) return;
    const rect = columns.getBoundingClientRect();
    const width = Math.round(event.clientX - rect.left);
    listWidth = Math.min(LIST_WIDTH_MAX, Math.max(LIST_WIDTH_MIN, width));
  }

  function onDividerPointerUp(event: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    const target = event.currentTarget as HTMLElement | null;
    target?.releasePointerCapture?.(event.pointerId);
    savePrefs();
  }

  function onDividerKeydown(event: KeyboardEvent) {
    const step = event.shiftKey ? 60 : 20;
    if (event.key === "ArrowLeft") {
      listWidth = Math.max(LIST_WIDTH_MIN, listWidth - step);
    } else if (event.key === "ArrowRight") {
      listWidth = Math.min(LIST_WIDTH_MAX, listWidth + step);
    } else return;
    event.preventDefault();
    savePrefs();
  }

  // The Insights charts' drag divider: pointer drag, arrow keys, saved height.
  function onInsightsDividerPointerDown(event: PointerEvent) {
    insightsDragging = true;
    const target = event.currentTarget as HTMLElement | null;
    target?.setPointerCapture?.(event.pointerId);
  }

  function onInsightsDividerPointerMove(event: PointerEvent) {
    if (!insightsDragging || insightsBody === undefined) return;
    const rect = insightsBody.getBoundingClientRect();
    insightsHeight = clampInsightsHeight(Math.round(event.clientY - rect.top));
  }

  function onInsightsDividerPointerUp(event: PointerEvent) {
    if (!insightsDragging) return;
    insightsDragging = false;
    const target = event.currentTarget as HTMLElement | null;
    target?.releasePointerCapture?.(event.pointerId);
    savePrefs();
  }

  function onInsightsDividerKeydown(event: KeyboardEvent) {
    const step = event.shiftKey ? 60 : 20;
    const base = insightsHeight ?? insightsBody?.offsetHeight ?? 0;
    if (event.key === "ArrowDown") {
      insightsHeight = clampInsightsHeight(base + step);
    } else if (event.key === "ArrowUp") {
      insightsHeight = clampInsightsHeight(base - step);
    } else if (event.key === "Home") {
      resetInsightsHeight();
    } else return;
    event.preventDefault();
    savePrefs();
  }

  /** Drop the fixed height and return to the natural layout. */
  function resetInsightsHeight(): void {
    insightsHeight = null;
    savePrefs();
  }

  /** The largest charts height that still leaves the call list visible. */
  function insightsHeightMax(): number {
    const view = traceRoot?.getBoundingClientRect().height ?? 0;
    return view > 0 ? Math.min(INSIGHTS_HEIGHT_MAX, Math.round(view - 220)) : INSIGHTS_HEIGHT_MAX;
  }

  function clampInsightsHeight(height: number): number {
    return Math.max(INSIGHTS_HEIGHT_MIN, Math.min(insightsHeightMax(), height));
  }

  // Dragging the divider selects no text anywhere on the page.
  $effect(() => {
    if (!insightsDragging) return;
    document.body.classList.add("resizing");
    return () => document.body.classList.remove("resizing");
  });

  // Scale the plots so the charts' content tracks the dragged height.
  $effect(() => {
    const height = insightsHeight;
    const body = insightsBody;
    if (height === null || body === undefined) {
      plotScale = 1;
      appliedScale = 1;
      return;
    }
    let plotsSum = 0;
    for (const plot of body.querySelectorAll<HTMLElement>("[data-plot]")) {
      plotsSum += plot.offsetHeight;
    }
    // The content stack's own height: scrollHeight would clamp to the fixed
    // box height whenever the box is taller than the content.
    const natural = (body.firstElementChild as HTMLElement | null)?.offsetHeight ?? body.scrollHeight;
    if (plotsSum <= 0) {
      plotScale = 1;
      appliedScale = 1;
      return;
    }
    const plotsBase = plotsSum / appliedScale;
    const nonPlot = natural - plotsSum;
    appliedScale = Math.min(3, Math.max(0.5, (height - nonPlot) / plotsBase));
    plotScale = appliedScale;
  });

  function toLocalInput(ms: number | null): string {
    if (ms === null) return "";
    const at = new Date(ms);
    const local = new Date(at.getTime() - at.getTimezoneOffset() * 60_000);
    return local.toISOString().slice(0, 16);
  }

  onMount(() => {
    if (transport === null) return;
    const prefs = loadTracePrefs($app.projectId);
    if (prefs !== null) {
      const restored: TraceFilters = { ...DEFAULT_TRACE_FILTERS, ...prefs };
      filters = restored;
      insightsOpen = prefs.insightsOpen ?? true;
      if (typeof prefs.listWidth === "number") {
        listWidth = Math.min(LIST_WIDTH_MAX, Math.max(LIST_WIDTH_MIN, prefs.listWidth));
      }
      if (typeof prefs.insightsHeight === "number" && Number.isFinite(prefs.insightsHeight)) {
        insightsHeight = Math.min(INSIGHTS_HEIGHT_MAX, Math.max(INSIGHTS_HEIGHT_MIN, prefs.insightsHeight));
      }
    }
    // An entry hand-over wins over the saved filters when one is waiting.
    const entry = traceEntryFilters($app.traceEntry);
    // An entry point can hand over the call it has open; the tab opens it too.
    const wanted = entry?.selected ?? null;
    if (entry !== null) {
      filters = { ...filters, ...entry, selected: null };
      actions.clearTraceEntry();
    }
    // Options come from an unfiltered probe so the menus hold every actor.
    void transport
      .list({ limit: TRACE_PAGE_SIZE })
      .then((page) => rememberRows(page.rows))
      .catch(() => undefined);
    void loadSettings();
    void reload().then(() => openEntrySelection(wanted));
    const pollTimer = setInterval(() => void poll(), POLL_MS);
    const detailTimer = setInterval(() => void pollDetail(), DETAIL_POLL_MS);
    // The window width says nothing about the room the tab itself has: the
    // panel can hold it narrow inside a wide window, and the other way round.
    const measure = () => {
      const width = traceRoot?.clientWidth ?? 0;
      narrow = width > 0 && width < NARROW_VIEW_WIDTH;
    };
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    if (traceRoot) observer?.observe(traceRoot);
    return () => {
      clearInterval(pollTimer);
      clearInterval(detailTimer);
      observer?.disconnect();
      if (reloadTimer !== null) clearTimeout(reloadTimer);
    };
  });

  // Entry points can hand the tab filters while it is already open.
  $effect(() => {
    const entry = $app.traceEntry;
    if (entry === null) return;
    actions.clearTraceEntry();
    const patch = traceEntryFilters(entry);
    if (patch === null) return;
    const wanted = patch.selected ?? null;
    filters = { ...filters, ...patch, selected: null };
    selectedKey = null;
    detail = null;
    savePrefs();
    void reload().then(() => openEntrySelection(wanted));
  });
</script><section class="trace-view" aria-label="Trace" bind:this={traceRoot}>
  <header class="trace-head">
    <div>
      <h1>Trace</h1>
      <p class="head-line">Every model call in this project — the lead's and every worker's.</p>
    </div>
  </header>

  {#if transport === null}
    <div class="placeholder">
      <h2>Model-call tracing runs in the desktop app</h2>
      <p>Open this project in Mnemo2 to see every model call with its context, reasoning and cost.</p>
    </div>
  {:else}
    <div class="trace-toolbar">
      <label class="filter">
        Agent
        <select value={filters.agent} onchange={(event) => setFilter("agent", event.currentTarget.value)}>
          {#each agentOptions as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
        </select>
      </label>
      <label class="filter">
        Model
        <select value={filters.model} onchange={(event) => setFilter("model", event.currentTarget.value)}>
          {#each modelOptions as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
        </select>
      </label>
      <label class="filter">
        Status
        <select value={filters.status} onchange={(event) => setFilter("status", event.currentTarget.value)}>
          {#each statusOptions as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
        </select>
      </label>
      <label class="filter">
        Time
        <select value={filters.time} onchange={(event) => setTime(event.currentTarget.value as TraceFilters["time"])}>
          {#each TRACE_TIME_OPTIONS as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
        </select>
      </label>
      {#if filters.time === "custom"}
        <label class="filter date">From <input type="datetime-local" value={toLocalInput(filters.from_ms)} onchange={(event) => setCustomFrom(event.currentTarget.value)} /></label>
        <label class="filter date">To <input type="datetime-local" value={toLocalInput(filters.to_ms)} onchange={(event) => setCustomTo(event.currentTarget.value)} /></label>
      {/if}
      <label class="filter">
        Purpose
        <select value={filters.purpose} onchange={(event) => setFilter("purpose", event.currentTarget.value)}>
          <option value="all">All purposes</option>
          {#each TRACE_PURPOSE_OPTIONS as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
        </select>
      </label>
      <div class="search-box">
        <Search size={13} aria-hidden="true" />
        <input
          type="search"
          placeholder="Search calls…"
          aria-label="Search calls"
          value={filters.search}
          oninput={(event) => setSearch(event.currentTarget.value)}
        />
        <select
          aria-label="Search scope"
          value={filters.scope}
          onchange={(event) => {
            filters = { ...filters, scope: event.currentTarget.value as TraceSearchScope };
            savePrefs();
            if (filters.search.trim() !== "") scheduleReload(0);
          }}
        >
          {#each scopeOptions as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
        </select>
      </div>
      <label class="filter">
        Group
        <select value={filters.grouping} onchange={(event) => setGrouping(event.currentTarget.value as TraceGrouping)}>
          {#each groupingOptions as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
        </select>
      </label>
      <label class="follow-toggle">
        <input type="checkbox" checked={follow} onchange={(event) => toggleFollow(event.currentTarget.checked)} />
        Follow new calls
      </label>
    </div>

    <div class="data-bar" aria-label="Trace data controls">
      <div class="data-group" role="group" aria-label="Export model calls">
        <span class="data-label"><Download size={12} aria-hidden="true" /> Export</span>
        <button
          type="button"
          class="data-button"
          disabled={exporting !== null || selectedKey === null}
          title={selectedKey === null ? "Open a call first" : "Write this call to a JSONL bundle"}
          onclick={() => void exportCalls("call")}
        >{exporting === "call" ? "Exporting…" : "Current call"}</button>
        <button
          type="button"
          class="data-button"
          disabled={exporting !== null || pickedKeys.length === 0}
          title="Write the ticked calls to a JSONL bundle"
          onclick={() => void exportCalls("selected")}
        >Selected calls ({pickedKeys.length})</button>
        <button
          type="button"
          class="data-button"
          disabled={exporting !== null || rows.length === 0}
          title="Write every call matching the current filters"
          onclick={() => void exportCalls("filtered")}
        >{exporting === "filtered" ? "Exporting…" : `All filtered (${total})`}</button>
      </div>
      <label class="bodies-toggle" title="Off by default: prompts and responses can contain secrets and personal data.">
        <input
          type="checkbox"
          checked={settings?.log_bodies ?? false}
          disabled={settings === null || settingsBusy}
          onchange={() => void toggleLogBodies()}
        />
        Save full prompts and responses to disk (sensitive)
      </label>
      <div class="data-group maintenance" role="group" aria-label="Clear the trace logs">
        <button type="button" class="data-button danger" disabled={maintenanceBusy} onclick={() => (confirmClear = true)}>
          <Trash2 size={12} aria-hidden="true" /> Clear live calls
        </button>
        <button type="button" class="data-button danger" disabled={maintenanceBusy} onclick={() => (confirmDelete = true)}>
          <History size={12} aria-hidden="true" /> Delete retained history
        </button>
      </div>
    </div>

    {#if settings !== null}
      <p class="data-note">{liveSummary} · {journalSummary}</p>
    {/if}
    {#if settingsError !== ""}
      <p class="data-note error" role="alert">Could not read the trace settings: {settingsError}</p>
    {/if}

    {#if confirmClear}
      <div class="confirm-card" role="alertdialog" aria-label="Confirm clearing the live calls">
        <p>
          Clear the live call log? This drops the {settings?.ring_capacity ?? "live"} calls held in memory,
          including any prompt and response bodies they still hold. Retained history is not touched.
        </p>
        <div class="confirm-actions">
          <button type="button" class="data-button danger" disabled={maintenanceBusy} onclick={() => void clearLive()}>
            {maintenanceBusy ? "Clearing…" : "Yes, clear live calls"}
          </button>
          <button type="button" class="data-button quiet" onclick={() => (confirmClear = false)}>Cancel</button>
        </div>
      </div>
    {/if}

    {#if confirmDelete}
      <div class="confirm-card" role="alertdialog" aria-label="Confirm deleting retained history">
        <p>
          Delete the retained history? This removes {settings === null ? "the retained metadata" : formatBytes(settings.history_bytes)} of retained
          metadata from <code>.coding/logs</code>
          {#if retainedInView > 0}({retainedInView} retained call{retainedInView === 1 ? "" : "s"} in this view){/if}.
          Live calls and this session's log stay as they are. This cannot be undone.
        </p>
        <div class="confirm-actions">
          <button type="button" class="data-button danger" disabled={maintenanceBusy} onclick={() => void deleteHistory()}>
            {maintenanceBusy ? "Deleting…" : "Yes, delete retained history"}
          </button>
          <button type="button" class="data-button quiet" onclick={() => (confirmDelete = false)}>Cancel</button>
        </div>
      </div>
    {/if}

    {#if maintenanceNote !== ""}
      <p class="data-note" role="status">{maintenanceNote}</p>
    {/if}
    {#if maintenanceError !== ""}
      <p class="data-note error" role="alert">{maintenanceError}</p>
    {/if}

    {#if exportResult !== null}
      <div class="export-result" role="status">
        <p>
          Exported {exportResult.count} call{exportResult.count === 1 ? "" : "s"} to
          <code>{exportResult.path}</code>
          {#if !exportResult.includes_bodies}<span class="muted"> — metadata only, no prompt or response text</span>{/if}
        </p>
        <div class="export-actions">
          <button type="button" class="data-button" onclick={() => void copyExportPath()}>
            <Copy size={12} aria-hidden="true" /> {copiedPath ? "Path copied" : "Copy path"}
          </button>
          <button type="button" class="data-button" title="Open the export folder with the file selected" onclick={() => void revealExport()}>
            <FolderOpen size={12} aria-hidden="true" /> Reveal
          </button>
          <button
            type="button"
            class="data-button quiet"
            aria-label="Dismiss the export result"
            onclick={() => {
              exportResult = null;
              exportNote = "";
              exportError = "";
            }}
          ><X size={12} aria-hidden="true" /></button>
        </div>
      </div>
    {/if}
    {#if exportNote !== ""}
      <p class="data-note" role="status">{exportNote}</p>
    {/if}
    {#if exportError !== ""}
      <p class="data-note error" role="alert">{exportError}</p>
    {/if}

    <section class="insights">
      <button type="button" class="insights-toggle" aria-expanded={insightsOpen} onclick={toggleInsights}>
        {insightsOpen ? "Hide charts" : "Insights"}
      </button>
      {#if insightsOpen}
        <div
          class="insights-body"
          bind:this={insightsBody}
          style:height={insightsHeight === null ? undefined : `${insightsHeight}px`}
          style:overflow={insightsHeight === null ? undefined : "auto"}
        >
          <TraceCharts rows={rows} scopeLabel={insightsScopeLabel} plotScale={plotScale} onbucket={applyBucket} />
        </div>
        <button
          type="button"
          class="insights-divider"
          aria-label="Resize the charts (drag or arrow keys)"
          title="Drag or use arrow keys to resize the charts. Double-click resets."
          onpointerdown={onInsightsDividerPointerDown}
          onpointermove={onInsightsDividerPointerMove}
          onpointerup={onInsightsDividerPointerUp}
          onpointercancel={onInsightsDividerPointerUp}
          onkeydown={onInsightsDividerKeydown}
          ondblclick={resetInsightsHeight}
        ></button>
      {/if}
    </section>

    {#snippet listColumn()}
      <div class="list-column">
        {#if chips.length > 0}
          <div class="chip-row">
            {#each chips as chip (chip.key)}
              <span class="chip">
                {chip.label}
                <button type="button" class="chip-x" aria-label={`Remove filter ${chip.label}`} onclick={() => clearChip(chip.key)}>
                  <X size={11} aria-hidden="true" />
                </button>
              </span>
            {/each}
            <button type="button" class="chip-clear" onclick={clearFilters}>Clear filters</button>
          </div>
        {/if}

        <p class="summary" title={unpriced > 0 ? `${unpriced} matching call${unpriced === 1 ? "" : "s"} have no configured price` : undefined}>
          {summary}
        </p>
        {#if capped}
          <p class="capped-note">Showing first 1,000 matches; narrow the time range.</p>
        {/if}

        {#if listError !== "" && rows.length === 0}
          <div class="state-card" role="alert">
            <p>Could not read the model calls: {listError}</p>
            <button type="button" class="state-button" onclick={() => void reload()}>Try again</button>
          </div>
        {:else if loadingList && rows.length === 0}
          <p class="loading-note">Loading model calls…</p>
        {:else if rows.length === 0 && listError === "" && loadedOnce}
          {#if chips.length > 0}
            <div class="state-card">
              <p>No calls match these filters.</p>
            </div>
          {:else}
            <div class="state-card">
              <p>Model calls will appear here when the lead or a worker runs.</p>
              <button type="button" class="state-button" onclick={actions.openLead}>Open Lead</button>
            </div>
          {/if}
        {:else}
          {#if listError !== ""}
            <div class="state-card soft" role="alert">
              <p>The last refresh failed: {listError}</p>
              <button type="button" class="state-button" onclick={() => void reload()}>Try again</button>
            </div>
          {/if}
          <TraceList
            {groups}
            selected={selectedKey}
            {checked}
            {newCount}
            {hasMore}
            {loadingMore}
            {nowMs}
            onselect={selectRow}
            oncheck={toggleCheck}
            onloadmore={() => void loadMore()}
            onfollow={revealNewCalls}
          />
        {/if}
      </div>
    {/snippet}

    {#snippet detailColumn()}
      <div class="detail-column">
        {#if selectedKey === null}
          <div class="placeholder soft">
            <p>Select a call to read its full context, reasoning and cost.</p>
          </div>
        {:else if detail === null && detailError === ""}
          <div class="placeholder soft">
            <p>{detailLoading ? "Opening the call…" : "Loading…"}</p>
          </div>
        {:else if detail === null}
          <div class="state-card" role="alert">
            <p>{detailError}</p>
            <button type="button" class="state-button" onclick={() => selectedKey !== null && void loadDetail(selectedKey, true)}>Try again</button>
          </div>
        {:else}
          <TraceDetailPane
            row={selectedRow ?? detail}
            {detail}
            previous={previousDetail}
            previousRow={previousRow}
            {previousPending}
            {nowMs}
            onclose={closeDetail}
          />
        {/if}
      </div>
    {/snippet}

    {#if narrow}
      {#if selectedKey !== null && detail !== null}
        {@render detailColumn()}
      {:else}
        {@render listColumn()}
      {/if}
    {:else}
      <div class="trace-columns" bind:this={columns} style:grid-template-columns="minmax({LIST_WIDTH_MIN}px, {listWidth}px) 7px minmax(360px, 1fr)">
        {@render listColumn()}
        <button
          type="button"
          class="divider"
          aria-label="Resize the call list (drag or arrow keys)"
          title="Drag or use arrow keys to resize the call list"
          onpointerdown={onDividerPointerDown}
          onpointermove={onDividerPointerMove}
          onpointerup={onDividerPointerUp}
          onpointercancel={onDividerPointerUp}
          onkeydown={onDividerKeydown}
        ></button>
        {@render detailColumn()}
      </div>
    {/if}
  {/if}
</section><style>
  .trace-view { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .trace-head { display: flex; align-items: flex-end; justify-content: space-between; gap: 12px; padding: 22px 26px 10px; }
  .trace-head h1 { margin: 0; color: var(--ink); font-size: var(--text-page); font-weight: 700; }
  .head-line { margin: 3px 0 0; color: var(--muted); font-size: var(--text-body); }
  .placeholder { display: grid; place-content: center; gap: 6px; margin: 60px auto; max-width: 420px; padding: 22px 26px; border: 1px dashed var(--line-strong); border-radius: 14px; background: var(--surface-soft); text-align: center; }
  .placeholder h2 { margin: 0; color: var(--ink); font-size: var(--text-title); font-weight: 650; }
  .placeholder p, .placeholder.soft p { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.5; }
  .placeholder.soft { margin: auto; }
  .trace-toolbar { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 4px 26px 12px; }
  .filter { display: inline-flex; align-items: center; gap: 6px; color: var(--muted); font-size: var(--text-meta); font-weight: 600; }
  .filter select { padding: 5px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink); font-size: var(--text-meta); cursor: pointer; }
  .filter select:hover { border-color: var(--line-strong); }
  .filter select:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .filter input[type="datetime-local"] { padding: 4px 6px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink); font-size: var(--text-meta); }
  .search-box { display: inline-flex; align-items: center; gap: 6px; padding: 0 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--muted); }
  .search-box:focus-within { border-color: var(--accent); }
  .search-box input { min-width: 160px; padding: 5px 0; border: 0; background: transparent; color: var(--ink); font-size: var(--text-meta); outline: none; }
  .search-box select { padding: 5px 6px 5px 0; border: 0; background: transparent; color: var(--muted); font-size: var(--text-meta); cursor: pointer; }
  .follow-toggle { display: inline-flex; align-items: center; gap: 6px; margin-left: auto; color: var(--muted); font-size: var(--text-meta); font-weight: 600; cursor: pointer; }
  .follow-toggle input { accent-color: var(--accent-strong); }
  .trace-columns { display: grid; min-height: 0; flex: 1; align-items: stretch; }
  .divider { display: block; width: 100%; height: 100%; margin: 0; padding: 0; border: 0; background: transparent; cursor: col-resize; touch-action: none; }
  .divider::after { display: block; margin: 0 3px; height: 100%; border-left: 1px solid transparent; content: ""; }
  .divider:hover::after, .divider:focus-visible::after { border-left-color: var(--accent); }
  .divider:focus { outline: none; }
  .divider:focus-visible { outline: none; }
  .list-column { display: flex; min-width: 0; overflow: auto; flex-direction: column; padding: 0 0 14px; }
  .detail-column { display: flex; min-width: 0; overflow: hidden; flex-direction: column; }
  .chip-row { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; padding: 10px 12px 0; }
  .chip { display: inline-flex; align-items: center; gap: 4px; padding: 3px 4px 3px 9px; border: 1px solid var(--line); border-radius: 999px; background: var(--surface); color: var(--ink-soft); font-size: var(--text-meta); }
  .chip-x { display: inline-flex; padding: 2px; border: 0; border-radius: 999px; background: transparent; color: var(--muted); cursor: pointer; }
  .chip-x:hover { color: var(--danger); background: var(--surface-soft); }
  .chip-clear { padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: var(--text-meta); font-weight: 600; }
  .chip-clear:hover { text-decoration: underline; }
  .summary { margin: 10px 12px 4px; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); font-variant-numeric: tabular-nums; }
  .capped-note { margin: 0 12px 4px; color: var(--warning); font-size: var(--text-meta); }
  .loading-note { margin: 16px 12px; color: var(--muted); font-size: var(--text-body); }
  .state-card { display: grid; gap: 8px; margin: 14px 12px; padding: 14px 16px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface-soft); }
  .state-card.soft { border-style: dashed; }
  .state-card p { margin: 0; color: var(--ink-soft); font-size: var(--text-body); line-height: 1.5; }
  .state-button { justify-self: start; padding: 6px 12px; border: 1px solid var(--accent); border-radius: 8px; background: var(--surface); color: var(--accent-strong); cursor: pointer; font-size: var(--text-meta); font-weight: 650; }
  .state-button:hover { border-color: var(--accent-strong); }
  .insights { padding: 12px 26px 20px; }
  .insights-toggle { padding: 6px 12px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: var(--text-meta); font-weight: 650; }
  .insights-toggle:hover { border-color: var(--accent); color: var(--accent-strong); }
  .insights-body { margin-top: 10px; }
  .insights-divider { display: block; width: 100%; height: 12px; margin: 2px 0 0; padding: 0; border: 0; background: transparent; cursor: row-resize; touch-action: none; }
  .insights-divider::after { display: block; margin: 5px 0; height: 2px; border-top: 1px solid transparent; content: ""; }
  .insights-divider:hover::after, .insights-divider:focus-visible::after { border-top-color: var(--accent); }
  .insights-divider:focus { outline: none; }
  .insights-divider:focus-visible { outline: none; }
  :global(body.resizing) { user-select: none; }
  .data-bar { display: flex; flex-wrap: wrap; align-items: center; gap: 10px 16px; padding: 8px 26px 4px; border-top: 1px solid var(--line); }
  .data-group { display: inline-flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .data-group.maintenance { margin-left: auto; }
  .data-label { display: inline-flex; align-items: center; gap: 5px; color: var(--muted); font-size: var(--text-meta); font-weight: 650; text-transform: uppercase; letter-spacing: 0.04em; }
  .data-button { display: inline-flex; align-items: center; gap: 5px; padding: 5px 10px; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: var(--text-meta); font-weight: 650; }
  .data-button:hover:not(:disabled) { border-color: var(--accent); color: var(--accent-strong); }
  .data-button:disabled { color: var(--disabled-ink); border-color: var(--line); cursor: default; }
  .data-button.danger { color: var(--danger); border-color: var(--line-strong); }
  .data-button.danger:hover:not(:disabled) { border-color: var(--danger); background: var(--danger-soft); color: var(--danger); }
  .data-button.quiet { border-color: transparent; background: transparent; }
  .data-button:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .bodies-toggle { display: inline-flex; align-items: center; gap: 6px; color: var(--ink-soft); font-size: var(--text-meta); font-weight: 600; cursor: pointer; }
  .data-note { margin: 2px 26px; color: var(--muted); font-size: var(--text-meta); line-height: 1.5; }
  .data-note.error { color: var(--danger); }
  .confirm-card { display: grid; gap: 8px; margin: 6px 26px; padding: 12px 14px; border: 1px solid var(--danger); border-radius: 12px; background: var(--danger-soft); }
  .confirm-card p { margin: 0; color: var(--ink-soft); font-size: var(--text-body); line-height: 1.5; }
  .confirm-card code { font-family: var(--font-mono); font-size: var(--text-meta); }
  .confirm-actions { display: flex; gap: 6px; }
  .export-result { display: grid; gap: 7px; margin: 6px 26px; padding: 11px 14px; border: 1px solid var(--accent-line); border-radius: 12px; background: var(--accent-soft); }
  .export-result p { margin: 0; color: var(--ink-soft); font-size: var(--text-body); line-height: 1.5; }
  .export-result code { display: inline-block; max-width: 100%; overflow-wrap: anywhere; color: var(--ink); font-family: var(--font-mono); font-size: var(--text-meta); }
  .export-result .muted { color: var(--muted); }
  .export-actions { display: flex; gap: 6px; flex-wrap: wrap; }
</style>