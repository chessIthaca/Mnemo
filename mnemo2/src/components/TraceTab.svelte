<script lang="ts">
  import { ArrowLeft, RotateCw, Trash2 } from "@lucide/svelte";
  import { onMount } from "svelte";
  import {
    capText,
    createTraceTransport,
    formatDuration,
    formatTokens,
    prettyJson,
    requestView,
    responseText,
    traceClock,
    traceStatus,
    type TraceDetail,
    type TracePage,
  } from "../lib/trace";

  /** How often the open tab re-reads the list: the poll carries no payloads. */
  const POLL_MS = 2_000;

  const transport = createTraceTransport();

  let page = $state<TracePage | null>(null);
  let listError = $state("");
  let polling = $state(false);

  let selectedId = $state<number | null>(null);
  let detail = $state<TraceDetail | null>(null);
  let detailError = $state("");
  let loadingDetail = $state(false);
  /** The version of the record `detail` holds, so a poll can skip a refetch. */
  let detailVersion = -1;

  /** Section keys whose "Show all" was pressed. */
  let uncapped = $state<Record<string, boolean>>({});
  let confirmingClear = $state(false);

  /** Flatten a rejected invoke (string or Error) to display text. */
  const errorText = (thrown: unknown) =>
    typeof thrown === "string"
      ? thrown
      : thrown instanceof Error
        ? thrown.message
        : "Could not read the request log";

  const rows = $derived(page?.requests ?? []);
  const selectedRow = $derived(rows.find((row) => row.id === selectedId) ?? null);
  const view = $derived(detail === null ? null : requestView(detail));
  const response = $derived(detail === null ? "" : responseText(detail));

  /** The collapsed blocks of the open request, in reading order. */
  const payloads = $derived.by(() => {
    if (view === null) return [];
    const blocks: { key: string; title: string; text: string }[] = [];
    view.system.forEach((prompt, index) => {
      blocks.push({
        key: `system-${index}`,
        title: view.system.length > 1 ? `System prompt ${index + 1}` : "System prompt",
        text: prompt,
      });
    });
    view.messages.forEach((message, index) => {
      blocks.push({ key: `message-${index}`, title: message.role, text: message.text });
    });
    if (view.tools !== null) {
      blocks.push({
        key: "tools",
        title: `Tool schemas (${view.tools.names.length}): ${view.tools.names.join(", ")}`,
        text: view.tools.json,
      });
    }
    if (view.params !== "") {
      blocks.push({ key: "params", title: "Request parameters", text: view.params });
    }
    blocks.push({ key: "request", title: "Exact request body", text: view.raw });
    return blocks;
  });

  /** One payload clamped for display, honouring a pressed "Show all". */
  function shown(key: string, text: string) {
    if (uncapped[key] === true) return { text, capped: false, total: text.length };
    return capText(text);
  }

  function showAll(key: string) {
    uncapped = { ...uncapped, [key]: true };
  }

  function closeDetail() {
    selectedId = null;
    detail = null;
    detailError = "";
    detailVersion = -1;
    uncapped = {};
  }

  async function loadDetail(id: number) {
    const active = transport;
    if (!active) return;
    loadingDetail = true;
    try {
      const next = await active.detail(id);
      if (next === null) {
        detail = null;
        detailVersion = -1;
        detailError = "This request has been evicted from the log.";
      } else {
        detail = next;
        detailVersion = next.version;
        detailError = "";
        uncapped = {};
      }
    } catch (thrown) {
      detail = null;
      detailVersion = -1;
      detailError = errorText(thrown);
    } finally {
      loadingDetail = false;
    }
  }

  /** Read the list; an open detail follows its row (refetch only on growth). */
  async function poll() {
    const active = transport;
    if (!active) return;
    polling = true;
    try {
      const next = await active.list();
      page = next;
      listError = "";
      if (selectedId !== null) {
        const row = next.requests.find((entry) => entry.id === selectedId) ?? null;
        if (row === null) closeDetail();
        else if (row.version !== detailVersion) await loadDetail(selectedId);
      }
    } catch (thrown) {
      listError = errorText(thrown);
    } finally {
      polling = false;
    }
  }

  function open(id: number) {
    selectedId = id;
    detail = null;
    detailError = "";
    detailVersion = -1;
    uncapped = {};
    confirmingClear = false;
    void loadDetail(id);
  }

  /** Two steps: the first arms the button, the second clears the log. */
  async function clearLog() {
    const active = transport;
    if (!active) return;
    if (!confirmingClear) {
      confirmingClear = true;
      return;
    }
    confirmingClear = false;
    try {
      await active.clear();
      closeDetail();
      await poll();
    } catch (thrown) {
      listError = errorText(thrown);
    }
  }

  onMount(() => {
    void poll();
    const timer = setInterval(() => void poll(), POLL_MS);
    return () => clearInterval(timer);
  });
</script>

<section class="trace-tab" aria-label="Trace">
  <header class="trace-header">
    <div>
      <p>MODEL REQUESTS — READ-ONLY</p>
      <h2>Trace</h2>
    </div>
    <div class="trace-actions">
      <button
        class="button button-quiet"
        aria-label="Refresh the request log"
        disabled={!transport}
        onclick={() => void poll()}
      >
        <RotateCw size={12} /> Refresh
      </button>
      <button
        class="button button-quiet danger"
        aria-label={confirmingClear ? "Confirm clearing the request log" : "Clear the request log"}
        disabled={!transport || rows.length === 0}
        onclick={() => void clearLog()}
      >
        <Trash2 size={12} /> {confirmingClear ? "Clear all?" : "Clear"}
      </button>
    </div>
  </header>

  <p class="trace-scope">
    Every model request this project's engine made — the lead's turns and every worker's.
    Newest first.{#if page} The log keeps the last {page.capacity}.{/if}
  </p>

  {#if !transport}
    <p class="trace-note">The Trace tab needs the desktop app.</p>
  {:else if listError}
    <div class="trace-error" role="alert">{listError}</div>
  {:else if selectedId !== null}
    <div class="detail">
      <div class="detail-head">
        <button class="button button-quiet" aria-label="Back to the request list" onclick={closeDetail}>
          <ArrowLeft size={12} /> Requests
        </button>
        {#if selectedRow}
          <code>{selectedRow.model}</code>
          <span>{traceClock(selectedRow.ts_ms)}</span>
        {/if}
      </div>

      {#if loadingDetail && detail === null}
        <p class="trace-note" role="status">Reading the request…</p>
      {:else if detailError}
        <div class="trace-error" role="alert">{detailError}</div>
      {:else if detail && view}
        <dl class="meta">
          <div>
            <dt>Status</dt>
            <dd>
              <span class="status" data-tone={traceStatus(detail).tone}>{traceStatus(detail).label}</span>
            </dd>
          </div>
          <div><dt>Endpoint</dt><dd>{detail.provider} · {detail.base_url}</dd></div>
          <div>
            <dt>Tokens</dt>
            <dd>
              {detail.usage
                ? `${formatTokens(detail.usage.prompt)} in · ${formatTokens(detail.usage.completion)} out`
                : "—"}
            </dd>
          </div>
          <div>
            <dt>Duration</dt>
            <dd>
              {formatDuration(detail.generation_ms)}
              <span class="dim">(ttft {formatDuration(detail.ttft_ms)})</span>
            </dd>
          </div>
          {#if detail.finish_reason !== null}
            <div><dt>Finish</dt><dd>{detail.finish_reason}</dd></div>
          {/if}
        </dl>

        {#if detail.error !== null}
          <div class="trace-error" role="alert">{detail.error}</div>
        {/if}
        {#if detail.cancelled}
          <p class="trace-note">The turn was cancelled — this is not a provider failure.</p>
        {/if}
        {#if detail.response_evicted}
          <p class="trace-note">The raw response was evicted from the log's memory budget; the status and timings remain.</p>
        {/if}
        {#if detail.request_truncated}
          <p class="trace-note">The stored request body was capped; its longest leaves were cut.</p>
        {/if}

        <h3>Request</h3>
        {#each payloads as payload (payload.key)}
          {@const body = shown(payload.key, payload.text)}
          <details class="payload">
            <summary>{payload.title}</summary>
            {#if body.text === ""}
              <p class="trace-note">This block was empty.</p>
            {:else}
              <pre>{body.text}</pre>
            {/if}
            {#if body.capped}
              <button class="button button-quiet" onclick={() => showAll(payload.key)}>
                Show all {body.total} characters
              </button>
            {/if}
          </details>
        {/each}

        <h3>Response</h3>
        {#if detail.raw_tool_calls !== null && detail.raw_tool_calls.length > 0}
          <details class="payload">
            <summary>Delivered tool calls ({detail.raw_tool_calls.length})</summary>
            <pre>{prettyJson(detail.raw_tool_calls)}</pre>
          </details>
        {/if}
        {#if response === ""}
          <p class="trace-note">
            {detail.response_evicted
              ? "No payload kept — it was evicted from the log's memory budget."
              : "No response bytes were recorded."}
          </p>
        {:else}
          {@const body = shown("response", response)}
          <details class="payload">
            <summary>
              Raw response{detail.response_truncated ? " — cut at the log's 2 MiB cap" : ""}
            </summary>
            <pre>{body.text}</pre>
            {#if body.capped}
              <button class="button button-quiet" onclick={() => showAll("response")}>
                Show all {body.total} characters
              </button>
            {/if}
          </details>
        {/if}
      {:else}
        <p class="trace-note" role="status">Reading the request…</p>
      {/if}
    </div>
  {:else}
    <ul class="requests" aria-label="Recent model requests">
      {#each [...rows].reverse() as row (row.id)}
        {@const status = traceStatus(row)}
        <li>
          <button
            class="request"
            aria-label={`Request ${row.id}: ${row.model}`}
            onclick={() => open(row.id)}
          >
            <span class="when">{traceClock(row.ts_ms)}</span>
            <span class="what">
              <span class="top">
                <span class="model">{row.model}</span>
                <span class="status" data-tone={status.tone}>{status.label}</span>
              </span>
              <span class="sub">
                {row.provider} · {row.usage
                  ? `${formatTokens(row.usage.prompt)} in · ${formatTokens(row.usage.completion)} out`
                  : "no usage"} · {formatDuration(row.generation_ms)}
              </span>
            </span>
          </button>
        </li>
      {/each}
      {#if rows.length === 0}
        <li>
          <p class="trace-note" role="status">
            {polling ? "Reading the request log…" : "No requests recorded yet."}
          </p>
        </li>
      {/if}
    </ul>
  {/if}
</section>

<style>
  .trace-tab { padding: 18px; }
  .trace-header { display: flex; align-items: flex-end; justify-content: space-between; gap: 12px; margin-bottom: 8px; }
  .trace-header p { margin: 0 0 4px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.11em; }
  .trace-header h2 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 550; }
  .trace-actions { display: flex; flex-wrap: wrap; gap: 6px; justify-content: flex-end; }
  .button.danger { border-color: var(--danger-line); color: var(--danger); }
  .button.danger:hover { background: var(--danger-soft); }
  .trace-scope { margin: 0 0 12px; color: var(--muted); font-size: var(--text-meta); line-height: 1.6; }
  .trace-note { margin: 10px 2px; color: var(--muted); font-size: var(--text-body); line-height: 1.6; }
  .trace-error { margin-bottom: 12px; padding: 10px 12px; border: 1px solid var(--danger-line); border-radius: 8px; background: var(--danger-soft); color: var(--danger); font-size: var(--text-body); line-height: 1.5; }
  .requests { display: flex; margin: 0; padding: 0; flex-direction: column; gap: 4px; list-style: none; }
  .request { display: grid; width: 100%; grid-template-columns: 60px minmax(0, 1fr); align-items: baseline; gap: 8px; padding: 7px 8px; border: 1px solid transparent; border-radius: 8px; background: transparent; color: var(--ink-soft); text-align: left; cursor: pointer; }
  .request:hover { border-color: var(--line); background: var(--surface-soft); }
  .request:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .when { color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); }
  .what { display: flex; min-width: 0; flex-direction: column; gap: 2px; }
  .top { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .model { overflow: hidden; color: var(--ink); font-family: var(--font-mono); font-size: var(--text-body); text-overflow: ellipsis; white-space: nowrap; }
  .sub { overflow: hidden; color: var(--muted); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .status { flex: none; padding: 1px 6px; border: 1px solid var(--line); border-radius: 999px; color: var(--muted); font-size: var(--text-meta); white-space: nowrap; }
  .status[data-tone="ok"] { border-color: var(--accent-line); background: var(--accent-soft); color: var(--accent-strong); }
  .status[data-tone="error"] { border-color: var(--danger-line); background: var(--danger-soft); color: var(--danger); }
  .status[data-tone="live"] { border-color: var(--accent-line); background: var(--running-bg); color: var(--running-ink); }
  .status[data-tone="stopped"] { border-color: var(--warning-line); background: var(--warning-soft); color: var(--warning); }
  .detail-head { display: flex; align-items: center; gap: 8px; margin-bottom: 12px; }
  .detail-head code { flex: 1; overflow: hidden; color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .detail-head span { color: var(--muted); font-size: var(--text-meta); white-space: nowrap; }
  .meta { display: flex; margin: 0 0 12px; padding: 0; flex-direction: column; gap: 4px; }
  .meta div { display: grid; grid-template-columns: 74px minmax(0, 1fr); gap: 10px; font-size: var(--text-body); }
  .meta dt { color: var(--muted); }
  .meta dd { margin: 0; overflow-wrap: anywhere; color: var(--ink-soft); }
  .dim { color: var(--muted); }
  .detail h3 { margin: 16px 0 8px; color: var(--muted); font-size: var(--text-meta); font-weight: 600; letter-spacing: 0.09em; text-transform: uppercase; }
  .payload { margin: 0 0 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); }
  .payload summary { padding: 7px 10px; color: var(--ink-soft); font-size: var(--text-body); cursor: pointer; }
  .payload summary:hover { color: var(--ink); }
  .payload summary:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .payload pre { max-height: 48vh; margin: 0; overflow: auto; padding: 0 10px 10px; color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-meta); line-height: 1.55; tab-size: 2; white-space: pre-wrap; overflow-wrap: anywhere; }
  .payload .button { margin: 0 10px 10px; }
  .payload .trace-note { padding: 0 10px 10px; margin: 0; }
  @media (max-width: 480px) { .trace-tab { padding: 14px; } }
</style>
