<script lang="ts">
  // Copyright (c) 2026 Carsten Hess
  // SPDX-License-Identifier: MIT
  // See LICENSE in the repository root.

  import { ArrowLeft } from "@lucide/svelte";
  import MetricTip from "./MetricTip.svelte";
  import TraceContext from "./TraceContext.svelte";
  import TraceOverview from "./TraceOverview.svelte";
  import TraceResponse from "./TraceResponse.svelte";
  import {
    actorLabel,
    callStatus,
    capText,
    prettyJson,
    purposeLabel,
    shortTurnId,
    traceClockMs,
    traceKey,
    type TraceDetail,
    type TraceRow,
  } from "../lib/trace";

  /**
   * The selected call: identity and status up top, then the detail's five
   * tabs — Overview by default, Context, Response, Tools, and Raw last.
   * Tabs return to Overview whenever the selection changes.
   */
  let {
    row,
    detail,
    previous,
    previousRow,
    previousPending,
    nowMs,
    onclose,
  }: {
    /** The live row, so status and tokens keep updating while the call streams. */
    row: TraceRow;
    detail: TraceDetail;
    /** The previous comparable call, for the Context tab's prefix comparison. */
    previous: TraceDetail | null;
    previousRow: TraceRow | null;
    previousPending: boolean;
    nowMs: number;
    onclose: () => void;
  } = $props();

  type Tab = "overview" | "context" | "response" | "tools" | "raw";
  let tab = $state<Tab>("overview");
  let lastKey = "";
  let rawExpanded = $state({ request: false, response: false });

  const status = $derived(callStatus(row));
  const turn = $derived(row.attribution?.turn_id ?? null);
  const contextCount = $derived(messageCount(detail));
  const toolCount = $derived(detail.raw_tool_calls === null ? 0 : detail.raw_tool_calls.length);
  const cappedRequest = $derived(capText(prettyJson(detail.request_json)));
  const cappedResponse = $derived(capText(detail.response_raw ?? ""));
  const toolEntries = $derived(
    (detail.raw_tool_calls ?? []).map((call, index) => ({
      id: `${call.id}-${index}`,
      call,
      args: parsedArgs(call.arguments),
    })),
  );

  function messageCount(source: TraceDetail): number {
    const request = source.request_json;
    if (request === null || typeof request !== "object") return 0;
    const messages = (request as Record<string, unknown>).messages;
    return Array.isArray(messages) ? messages.length : 0;
  }

  /** Tool arguments as parsed JSON when valid, raw text otherwise. */
  function parsedArgs(raw: string): { pretty: string; parsed: boolean } {
    try {
      return { pretty: prettyJson(JSON.parse(raw)), parsed: true };
    } catch {
      return { pretty: raw, parsed: false };
    }
  }

  // Tabs return to Overview whenever the selection changes.
  $effect(() => {
    const key = traceKey(row);
    if (key !== lastKey) {
      lastKey = key;
      tab = "overview";
      rawExpanded = { request: false, response: false };
    }
  });

  const tabs = $derived<{ value: Tab; label: string }[]>([
    { value: "overview", label: "Overview" },
    { value: "context", label: `Context${contextCount > 0 ? ` (${contextCount})` : ""}` },
    { value: "response", label: "Response" },
    { value: "tools", label: `Tools${toolCount > 0 ? ` (${toolCount})` : ""}` },
    { value: "raw", label: "Raw" },
  ]);
</script>

<div class="trace-detail">
  <header class="detail-head">
    <button class="icon-button back" type="button" aria-label="Back to the list" title="Back to the list" onclick={onclose}>
      <ArrowLeft size={14} />
    </button>
    <div class="detail-title">
      <p class="detail-line">
        <span class="dot {status.tone}" aria-hidden="true"></span>
        <strong>{actorLabel(row.attribution)}</strong>
        {#if turn !== null}<span class="turn" title={turn}>· Turn {shortTurnId(turn)}</span>{/if}
      </p>
      <p class="detail-line muted">
        {traceClockMs(row.ts_ms)} · request #{row.id}
        · {row.model}
        · {row.attribution === null ? "Unknown purpose" : purposeLabel(row.attribution.purpose)}
      </p>
    </div>
    <span class="status {status.tone}">{status.label}</span>
  </header>

  <div class="tabs" role="group" aria-label="Detail sections">
    {#each tabs as entry (entry.value)}
      <button
        type="button"
        class="tab"
        aria-pressed={tab === entry.value}
        class:active={tab === entry.value}
        onclick={() => (tab = entry.value)}
      >{entry.label}</button>
    {/each}
  </div>

  <div class="detail-body">
    {#if tab === "overview"}
      <TraceOverview {row} {detail} {nowMs} />
    {:else if tab === "context"}
      <TraceContext {detail} {previous} {previousRow} {previousPending} />
    {:else if tab === "response"}
      <TraceResponse {row} {detail} />
    {:else if tab === "tools"}
      <div class="tools-tab">
        {#if row.retained}
          <p class="tab-note"><strong>Retained metadata.</strong> This call survived a restart without its prompt or response. Enable full-body logging for future calls.</p>
        {:else if detail.raw_tool_calls === null || detail.raw_tool_calls.length === 0}
          <p class="tab-note">This response requested no tool calls.</p>
        {:else}
          <ol class="tool-calls">
            {#each toolEntries as entry (entry.id)}
              <li class="tool-call">
                <p class="tool-name">{entry.call.name}</p>
                <pre class:unparsed={!entry.args.parsed}>{entry.args.pretty}</pre>
                {#if entry.call.truncated}
                  <p class="tab-note">The recorded arguments were truncated in the log.</p>
                {/if}
              </li>
            {/each}
          </ol>
        {/if}
      </div>
    {:else}
      <div class="raw-tab">
        {#if row.retained}
          <p class="tab-note"><strong>Retained metadata.</strong> No request or response bodies were kept for this call.</p>
        {:else}
          <section class="raw-block">
            <h4>Request <span class="raw-meta">{detail.request_truncated ? "· truncated in the log" : ""}</span></h4>
            <pre>{rawExpanded.request ? prettyJson(detail.request_json) : cappedRequest.text}</pre>
            {#if cappedRequest.capped && !rawExpanded.request}
              <button class="link-button" type="button" onclick={() => (rawExpanded.request = true)}>
                Show all {cappedRequest.total.toLocaleString()} characters
              </button>
            {/if}
          </section>
          <section class="raw-block">
            <h4>Response {#if detail.response_evicted}<span class="raw-meta">· evicted</span>{/if}{#if row.response_truncated}<span class="raw-meta">· truncated</span>{/if}</h4>
            {#if detail.response_evicted}
              <p class="tab-note">The response text was evicted from the ring.</p>
            {:else}
              <pre>{rawExpanded.response ? (detail.response_raw ?? "") : cappedResponse.text}</pre>
              {#if cappedResponse.capped && !rawExpanded.response}
                <button class="link-button" type="button" onclick={() => (rawExpanded.response = true)}>
                  Show all {cappedResponse.total.toLocaleString()} characters
                </button>
              {/if}
            {/if}
          </section>
        {/if}
        {#if row.guard_cut !== null}
          <section class="raw-block">
            <h4>Guard cut <MetricTip term="guard_cut" /></h4>
            <pre>{prettyJson(row.guard_cut)}</pre>
          </section>
        {/if}
        <section class="raw-block">
          <h4>Record <MetricTip term="http" label="Record" /></h4>
          <dl class="raw-list">
            <div><dt>Request</dt><dd>#{row.id} · version {row.version}</dd></div>
            <div><dt>Run</dt><dd title={row.run_id}>{shortTurnId(row.run_id)}</dd></div>
            <div><dt>Endpoint</dt><dd>{row.base_url}</dd></div>
            <div><dt>Provider</dt><dd>{row.provider}</dd></div>
            <div><dt>HTTP</dt><dd>{row.http_status === null ? "—" : row.http_status}</dd></div>
          </dl>
        </section>
      </div>
    {/if}
  </div>
</div>

<style>
  .trace-detail { display: grid; grid-template-rows: auto auto 1fr; min-height: 0; height: 100%; }
  .detail-head { display: flex; align-items: flex-start; gap: 8px; padding: 14px 16px 10px; }
  .back { display: none; }
  .detail-title { flex: 1; display: grid; gap: 2px; min-width: 0; }
  .detail-line { display: flex; align-items: center; gap: 7px; margin: 0; color: var(--ink); font-size: var(--text-body); line-height: 1.4; }
  .detail-line strong { font-weight: 650; }
  .detail-line.muted { color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); font-variant-numeric: tabular-nums; }
  .dot { flex: 0 0 auto; width: 8px; height: 8px; border-radius: 999px; }
  .dot.ok { background: var(--stat-add); }
  .dot.live { background: var(--running-ink); }
  .dot.error { background: var(--danger); }
  .dot.stopped { background: var(--muted); }
  .dot.guard { background: var(--warning); }
  .turn { color: var(--muted); }
  .status { flex: 0 0 auto; padding: 3px 10px; border-radius: 999px; font-size: var(--text-meta); font-weight: 650; }
  .status.ok { background: var(--stat-add-bg); color: var(--stat-add); }
  .status.live { background: var(--running-bg); color: var(--running-ink); }
  .status.error { background: var(--danger-bg); color: var(--danger); }
  .status.stopped { background: var(--surface-soft); color: var(--muted); }
  .status.guard { background: var(--warning-soft); color: var(--warning); }
  .tabs { display: flex; gap: 2px; padding: 0 16px; border-bottom: 1px solid var(--line); }
  .tab { padding: 8px 12px; border: 0; border-bottom: 2px solid transparent; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-body); font-weight: 600; }
  .tab:hover { color: var(--ink-soft); }
  .tab.active { border-bottom-color: var(--accent); color: var(--ink); }
  .tab:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .detail-body { overflow: auto; padding: 16px; }
  .tab-note { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.5; }
  .tab-note strong { color: var(--ink); }
  .tools-tab { display: grid; gap: 10px; }
  .tool-calls { display: grid; gap: 10px; margin: 0; padding: 0; list-style: none; }
  .tool-call { display: grid; gap: 6px; padding: 11px 13px; border: 1px solid var(--line); border-radius: 11px; background: var(--surface-soft); }
  .tool-name { margin: 0; color: var(--accent-strong); font-family: var(--font-mono); font-size: var(--text-body); font-weight: 650; }
  .tool-call pre { margin: 0; max-height: 300px; overflow: auto; padding: 9px 11px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-meta); line-height: 1.55; white-space: pre-wrap; overflow-wrap: anywhere; }
  .raw-tab { display: grid; gap: 16px; }
  .raw-block { display: grid; gap: 8px; }
  .raw-block h4 { display: flex; align-items: center; gap: 6px; margin: 0; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.04em; text-transform: uppercase; }
  .raw-meta { color: var(--warning); text-transform: none; }
  .raw-block pre { margin: 0; max-height: 420px; overflow: auto; padding: 11px 13px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-meta); line-height: 1.55; white-space: pre-wrap; overflow-wrap: anywhere; }
  .raw-list { display: grid; gap: 6px; margin: 0; }
  .raw-list > div { display: grid; grid-template-columns: minmax(110px, 30%) 1fr; gap: 10px; }
  .raw-list dt { color: var(--muted); font-size: var(--text-body); }
  .raw-list dd { margin: 0; color: var(--ink); font-family: var(--font-mono); font-size: var(--text-meta); overflow-wrap: anywhere; }
  .link-button { justify-self: start; padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: var(--text-meta); font-weight: 600; }
  .link-button:hover { text-decoration: underline; }
  @media (max-width: 900px) {
    .back { display: inline-flex; }
    .detail-body { padding: 12px; }
  }
</style>