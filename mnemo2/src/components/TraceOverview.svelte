<script lang="ts">
  // Copyright (c) 2026 Carsten Hess
  // SPDX-License-Identifier: MIT
  // See LICENSE in the repository root.

  import { CircleAlert, ShieldAlert, TriangleAlert } from "@lucide/svelte";
  import MetricTip from "./MetricTip.svelte";
  import {
    actorKindLabel,
    actorLabel,
    actorSubLabel,
    cacheReuse,
    callStatus,
    formatDuration,
    formatTokens,
    phaseTotalMs,
    purposeLabel,
    shortTurnId,
    traceAge,
    traceCostLabel,
    finishLabel,
    rowTokenSummary,
    traceClockMs,
    tracePhases,
    type TraceDetail,
    type TracePhase,
    type TraceRow,
  } from "../lib/trace";

  /**
   * The Overview tab: what happened, to whom, for how long and how much —
   * identity, status, cost, tokens, timing and the explanations that keep
   * partial data honest. Technical leftovers live in its advanced details.
   */
  let { row, detail, nowMs }: { row: TraceRow; detail: TraceDetail; nowMs: number } = $props();

  const status = $derived(callStatus(row));
  const usage = $derived(row.usage);
  const tokens = $derived(rowTokenSummary(row));
  const phases = $derived(tracePhases(row));
  const totalMs = $derived(phaseTotalMs(row));
  const barSegments = $derived(phases.filter((phase) => phase.ms !== null) as (TracePhase & { ms: number })[]);
  const attribution = $derived(row.attribution);
  const finish = $derived(finishLabel(row.finish_reason));
  const estimate = $derived(usage !== null && !row.is_complete);
  const reasoningTokens = $derived(usage === null ? null : usage.reasoning);
</script>

<div class="overview">
  <section class="overview-block">
    <h3>Who</h3>
    <dl class="overview-list">
      <div>
        <dt>Actor</dt>
        <dd>
          {actorLabel(attribution)}
          {#if attribution !== null}<span class="kind">({actorKindLabel(attribution.actor_kind)})</span>{/if}
        </dd>
      </div>
      {#if actorSubLabel(attribution) !== ""}
        <div><dt>Plan</dt><dd>{actorSubLabel(attribution)}</dd></div>
      {/if}
      {#if attribution !== null && attribution.turn_id !== null}
        <div>
          <dt>Turn</dt>
          <dd title={attribution.turn_id}>{shortTurnId(attribution.turn_id)}</dd>
        </div>
      {/if}
      <div><dt>Purpose</dt><dd>{attribution === null ? "Unknown" : purposeLabel(attribution.purpose)}</dd></div>
      <div>
        <dt>Started</dt>
        <dd title={traceAge(row.ts_ms, nowMs)}>{traceClockMs(row.ts_ms)} ({traceAge(row.ts_ms, nowMs)})</dd>
      </div>
    </dl>
  </section>

  <section class="overview-block">
    <h3>Status</h3>
    <p class="status-line"><span class="status-chip {status.tone}">{status.label}</span></p>
    {#if row.error !== null}
      <div class="notice danger"><CircleAlert size={13} /><div><strong>The call failed.</strong> {row.error}</div></div>
    {/if}
    {#if row.guard_cut !== null}
      <div class="notice guard">
        <ShieldAlert size={13} />
        <div>
          <strong>A client-side stream guard ended this call.</strong>
          Limit: <code>{row.guard_cut.boundary}</code>, stopped after {formatTokens(row.guard_cut.chars_before)} characters
          {#if row.guard_cut.prefix_empty} — the guard fired before any text arrived{/if}.
        </div>
      </div>
    {/if}
    {#if row.retained}
      <div class="notice quiet">
        <TriangleAlert size={13} />
        <div><strong>Retained metadata.</strong> This call survived a restart without its prompt or response. Enable full-body logging for future calls.</div>
      </div>
    {:else if row.response_evicted}
      <div class="notice quiet">
        <TriangleAlert size={13} />
        <div><strong>Response evicted.</strong> The recorded response text was dropped from memory to keep the log's budget; the metadata above survived.</div>
      </div>
    {:else if row.response_truncated}
      <div class="notice quiet">
        <TriangleAlert size={13} />
        <div><strong>Response truncated.</strong> The recorded response was cut at the log's per-call cap; Raw shows the kept part.</div>
      </div>
    {/if}
    {#if row.cancelled}
      <p class="status-note">The call was cancelled while it was still streaming.</p>
    {/if}
  </section>

  <section class="overview-block">
    <h3>Cost <MetricTip term="cost" label="Estimated cost" /></h3>
    {#if !row.priced}
      <p class="metric big">{traceCostLabel(row.cost_micros)}</p>
      <p class="status-note">No price is configured for {row.model}, so this call is not costed.</p>
    {:else}
      <p class="metric big">{traceCostLabel(row.cost_micros)}</p>
    {/if}
  </section>

  <section class="overview-block">
    <h3>Tokens {#if usage !== null}{#if estimate}<span class="estimate"><MetricTip term="live_estimate" label="Live estimate" /></span>{:else}<span class="estimate">Final</span>{/if}{/if}</h3>
    {#if usage === null}
      <p class="status-note">The provider did not report token usage for this call.</p>
    {:else}
      <dl class="overview-list">
        <div>
          <dt>Input <MetricTip term="input" /></dt>
          <dd>{formatTokens(usage.prompt)} <span class="cached">({formatTokens(usage.cached)} cached <MetricTip term="cached_input" label="cached" />)</span></dd>
        </div>
        <div>
          <dt>Output <MetricTip term="output" /></dt>
          <dd>
            {formatTokens(usage.completion)}
            {#if reasoningTokens !== null && reasoningTokens > 0}
              <span class="cached">({formatTokens(reasoningTokens)} reasoning <MetricTip term="reasoning" label="reasoning" />)</span>
            {/if}
          </dd>
        </div>
        <div>
          <dt>Cache reuse <MetricTip term="cache_reuse" /></dt>
          <dd>{cacheReuse(usage) === null ? "Not reported" : `${Math.round((cacheReuse(usage) ?? 0) * 100)}%`}</dd>
        </div>
      </dl>
    {/if}
  </section>

  <section class="overview-block">
    <h3>Timing <MetricTip term="total_time" label="Total time" /></h3>
    {#if totalMs === null}
      <p class="status-note">No phase timings were captured for this call.</p>
    {:else}
      <div class="phase-bar" role="img" aria-label={`Timing: ${formatDuration(totalMs)} in total`}>
        {#each barSegments as segment (segment.key)}
          <span class="phase-segment {segment.key}" style:width="{(segment.ms / totalMs) * 100}%"></span>
        {/each}
      </div>
      {#if row.connect_ms !== null || row.ttft_ms !== null}
        <div class="metric-chips">
          {#if row.connect_ms !== null}
            <span class="metric-chip">Connect <MetricTip term="connect" label="Connect" /> <strong>{formatDuration(row.connect_ms)}</strong></span>
          {/if}
          {#if row.ttft_ms !== null}
            <span class="metric-chip">First token <MetricTip term="first_token" label="First token" /> <strong>{formatDuration(row.ttft_ms)}</strong></span>
          {/if}
        </div>
      {/if}
      <dl class="overview-list phases">
        {#each phases as phase (phase.key)}
          {#if phase.ms !== null}
            <div>
              <dt>{phase.label} <MetricTip term={phase.key} /></dt>
              <dd>{formatDuration(phase.ms)}</dd>
            </div>
          {/if}
          {#each phase.inside ?? [] as inner (inner.key)}
            <div class="inside">
              <dt>↳ {inner.label} <MetricTip term={inner.key} /></dt>
              <dd>{formatDuration(inner.ms)}</dd>
            </div>
          {/each}
        {/each}
        {#if totalMs !== null}
          <div class="total">
            <dt>Total time <MetricTip term="total_time" /></dt>
            <dd>{formatDuration(totalMs)}</dd>
          </div>
        {/if}
        {#if row.tools_ms !== null}
          <div class="inside">
            <dt>Tools <MetricTip term="tools" /></dt>
            <dd>{formatDuration(row.tools_ms)} — after the response, not part of Total time</dd>
          </div>
        {/if}
      </dl>
    {/if}
  </section>

  <details class="overview-block advanced">
    <summary>Advanced details</summary>
    <dl class="overview-list">
      {#if finish !== null}
        <div><dt>Finish <MetricTip term="finish" /></dt><dd>{finish} <code>{row.finish_reason}</code></dd></div>
      {/if}
      {#if row.http_status !== null}
        <div><dt>HTTP <MetricTip term="http" /></dt><dd>{row.http_status}</dd></div>
      {/if}
      {#if attribution !== null}
        <div><dt>Session</dt><dd title={attribution.session_id}>{shortTurnId(attribution.session_id)}</dd></div>
      {/if}
      <div><dt>Request</dt><dd>#{row.id} · version {row.version} · run {shortTurnId(row.run_id)}</dd></div>
      <div><dt>Endpoint</dt><dd>{row.base_url}</dd></div>
      <div><dt>Provider</dt><dd>{row.provider}</dd></div>
    </dl>
  </details>
</div>

<style>
  .overview { display: grid; gap: 18px; }
  .overview-block { display: grid; gap: 8px; }
  .overview-block h3 { display: flex; align-items: center; gap: 6px; margin: 0; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.04em; text-transform: uppercase; }
  .overview-list { display: grid; gap: 6px; margin: 0; }
  .overview-list > div { display: grid; grid-template-columns: minmax(120px, 34%) 1fr; gap: 10px; align-items: baseline; }
  .overview-list dt { color: var(--muted); font-size: var(--text-body); }
  .overview-list dd { margin: 0; color: var(--ink); font-size: var(--text-body); overflow-wrap: anywhere; }
  .kind { margin-left: 6px; color: var(--muted); font-size: var(--text-meta); }
  .cached { color: var(--muted); font-size: var(--text-meta); }
  .metric.big { margin: 0; color: var(--ink); font-size: var(--text-title); font-weight: 650; font-variant-numeric: tabular-nums; }
  .estimate { color: var(--muted); font-size: var(--text-meta); font-weight: 600; }
  .status-line { margin: 0; }
  .status-chip { display: inline-flex; align-items: center; padding: 3px 9px; border-radius: 999px; font-size: var(--text-meta); font-weight: 650; }
  .status-chip.ok { background: var(--stat-add-bg); color: var(--stat-add); }
  .status-chip.live { background: var(--running-bg); color: var(--running-ink); }
  .status-chip.error { background: var(--danger-bg); color: var(--danger); }
  .status-chip.stopped { background: var(--surface-soft); color: var(--muted); }
  .status-chip.guard { background: var(--warning-soft); color: var(--warning); }
  .status-note { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.5; }
  .notice { display: flex; gap: 8px; padding: 9px 11px; border-radius: 10px; font-size: var(--text-body); line-height: 1.5; }
  .notice :global(svg) { flex: 0 0 auto; margin-top: 2px; }
  .notice.danger { background: var(--danger-bg); color: var(--danger-ink); }
  .notice.guard { background: var(--warning-soft); color: var(--ink); }
  .notice.quiet { background: var(--surface-soft); color: var(--ink-soft); }
  .notice strong { font-weight: 650; }
  .notice code { font-family: var(--font-mono); font-size: var(--text-meta); }
  .phase-bar { display: flex; height: 10px; overflow: hidden; border-radius: 5px; background: var(--line); }
  .metric-chips { display: flex; flex-wrap: wrap; gap: 6px; }
  .metric-chip { display: inline-flex; align-items: center; gap: 5px; padding: 3px 10px; border-radius: 999px; background: var(--surface-soft); color: var(--muted); font-size: var(--text-meta); }
  .metric-chip strong { color: var(--ink); font-weight: 650; font-variant-numeric: tabular-nums; }
  .phase-segment { height: 100%; min-width: 2px; }
  .phase-segment.setup { background: var(--ink-soft); }
  .phase-segment.upload { background: var(--warning); }
  .phase-segment.server { background: var(--running-ink); }
  .phase-segment.generation { background: var(--stat-add); }
  .phase-segment.retry_wait { background: var(--muted); }
  .overview-list .inside dt { padding-left: 14px; color: var(--muted); font-size: var(--text-meta); }
  .overview-list .inside dd { color: var(--ink-soft); font-size: var(--text-meta); }
  .overview-list .total { border-top: 1px solid var(--line); padding-top: 6px; }
  .overview-list .total dt { font-weight: 650; }
  .advanced summary { cursor: pointer; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.04em; text-transform: uppercase; }
  .advanced summary:hover { color: var(--ink-soft); }
  .advanced[open] summary { margin-bottom: 8px; }
</style>