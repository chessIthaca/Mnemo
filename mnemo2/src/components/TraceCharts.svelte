<script lang="ts">
  /**
   * The Trace page's Insights panel: one collapsible card with three charts —
   * where request time went, what tokens went to, and what calls cost over
   * time. Every chart is driven by the exact filtered `rows` the caller passes,
   * so the page and the compact side tab can share it without drifting.
   *
   * `onbucket` receives a cost bucket's half-open `[from_ms, to_ms)` range when
   * the user selects a column; the page turns that into its time filter.
   */
  import { ChartColumn } from "@lucide/svelte";
  import { formatEstimate } from "../lib/cost";
  import {
    TIMING_SEGMENTS,
    TOKEN_SEGMENTS,
    bucketCostLabel,
    costChart,
    timingChart,
    timingValues,
    tokenChart,
    tokenValues,
    type CostBucket,
    type CostGrouping,
    type TimingColumn,
    type TokenColumn,
    type TraceRow,
  } from "../lib/traceCharts";
  import { formatDuration, formatTokens } from "../lib/trace";
  import { formatTokenCount } from "../lib/usage";

  let {
    rows,
    scopeLabel,
    onbucket,
  }: {
    rows: TraceRow[];
    /** The filter summary, e.g. "this session" or "lead · last hour". */
    scopeLabel: string;
    /** Apply a cost bucket as the page's time filter (half-open range). */
    onbucket?: (from_ms: number, to_ms: number) => void;
  } = $props();

  type Tab = "timing" | "tokens" | "cost";

  let open = $state(true);
  let tab = $state<Tab>("timing");
  let grouping = $state<CostGrouping>("agent");
  let hiddenTiming = $state<string[]>([]);
  let hiddenTokens = $state<string[]>([]);
  let hiddenCost = $state<string[]>([]);

  const timing = $derived(timingChart(rows, new Set(hiddenTiming)));
  const tokens = $derived(tokenChart(rows, new Set(hiddenTokens)));
  const cost = $derived(costChart(rows, grouping, new Set(hiddenCost)));

  /** Fixed segment colours — theme tokens only, shared by bars, chips, tables. */
  const TIMING_COLORS: Record<string, string> = {
    retry: "var(--warning)",
    setup: "var(--ink-soft)",
    connect: "var(--running-ink)",
    firstToken: "var(--code-keyword)",
    generation: "var(--accent)",
    tools: "var(--event-review-ink)",
  };
  const TOKEN_COLORS: Record<string, string> = {
    uncachedInput: "var(--running-ink)",
    cachedInput: "var(--accent)",
    visibleOutput: "var(--code-keyword)",
    reasoningOutput: "var(--code-number)",
  };
  /** Cycled by series order so an agent always wears the same colour. */
  const SERIES_COLORS = [
    "var(--accent)",
    "var(--running-ink)",
    "var(--code-keyword)",
    "var(--warning)",
    "var(--code-number)",
    "var(--danger)",
    "var(--event-review-ink)",
    "var(--badge-decision-ink)",
    "var(--muted)",
  ];

  const seriesColors = $derived(
    new Map(cost.series.map((series, index) => [series.key, SERIES_COLORS[index % SERIES_COLORS.length]])),
  );

  /** Toggle one legend key in a hidden list (immutably, so Svelte reacts). */
  function toggle(hidden: string[], key: string): string[] {
    return hidden.includes(key) ? hidden.filter((item) => item !== key) : [...hidden, key];
  }

  /** One x-axis label every ~12 columns; each column keeps its full title. */
  function labelStep(count: number): number {
    return Math.max(1, Math.ceil(count / 12));
  }

  /** The visible timing segments of one column, bottom-up. */
  function visibleTiming(column: TimingColumn): { key: string; label: string; value: number }[] {
    const values = timingValues(column);
    return TIMING_SEGMENTS.filter(
      (segment) => !hiddenTiming.includes(segment.key) && values[segment.key] > 0,
    ).map((segment) => ({
      key: segment.key,
      label: segment.label,
      value: values[segment.key],
    }));
  }

  /** The visible token segments of one column, bottom-up. */
  function visibleTokens(column: TokenColumn): { key: string; label: string; value: number }[] {
    const values = tokenValues(column);
    return TOKEN_SEGMENTS.filter(
      (segment) => !hiddenTokens.includes(segment.key) && values[segment.key] > 0,
    ).map((segment) => ({
      key: segment.key,
      label: segment.label,
      value: values[segment.key],
    }));
  }

  /** The tooltip for one timing column, one phase per line. */
  function timingTip(column: TimingColumn): string {
    const lines = [`${column.clock} · ${column.status.label}`];
    for (const segment of TIMING_SEGMENTS) {
      const value = timingValues(column)[segment.key];
      if (value > 0) lines.push(`${segment.label}: ${formatDuration(value)}`);
    }
    if (column.compactionMs > 0) {
      lines.push(`Compaction (inside setup): ${formatDuration(column.compactionMs)}`);
    }
    if (column.stallMs > 0) {
      lines.push(`Stalls (inside generation): ${formatDuration(column.stallMs)}`);
    }
    return lines.join("\n");
  }

  /** The tooltip for one token column, one segment per line. */
  function tokenTip(column: TokenColumn): string {
    if (!column.hasUsage) return `${column.clock} · no token usage reported`;
    const lines = [
      `${column.clock} · ${formatTokenCount(column.totalTokens)} tokens`,
      `Input: ${formatTokenCount(column.prompt)} (${formatTokenCount(column.cached)} cached)`,
      `Output: ${formatTokenCount(column.completion)} (${formatTokenCount(column.reasoning)} reasoning)`,
    ];
    if (column.cachePct !== null) lines.push(`Cache reuse: ${column.cachePct}%`);
    return lines.join("\n");
  }

  /** A phase cell: its duration, or "—" when the phase did not happen. */
  function durationCell(value: number): string {
    return value > 0 ? formatDuration(value) : "—";
  }

  /** The tooltip/aria label of a cost bucket: range, calls, estimate, tokens. */
  function bucketTitle(bucket: CostBucket): string {
    const lines = [
      `${bucket.title} · ${bucket.calls} ${bucket.calls === 1 ? "call" : "calls"}`,
      `Estimated: ${bucketCostLabel(bucket)}`,
      `Tokens: ${formatTokenCount(bucket.tokens)}`,
    ];
    if (bucket.unpricedCalls > 0) {
      lines.push(
        `Unpriced: ${bucket.unpricedCalls} ${bucket.unpricedCalls === 1 ? "call" : "calls"}, ${formatTokenCount(bucket.unpricedTokens)} tokens`,
      );
    }
    if (bucket.liveCalls > 0) {
      lines.push(`Still running: ${bucket.liveCalls}`);
    }
    if (onbucket && bucket.calls > 0) lines.push("Select to filter to this range");
    return lines.join("\n");
  }

  /** A series' dollars in one bucket, or "—" when it had no priced call. */
  function seriesCost(bucket: CostBucket, key: string): string {
    const slice = bucket.seriesCosts.find((entry) => entry.key === key);
    return slice ? formatEstimate(slice.costMicros) : "—";
  }

  /** The readable name of the bucket width. */
  const unitLabel = $derived(
    cost.unit === "minute" ? "Minute buckets" : cost.unit === "hour" ? "Hour buckets" : "Day buckets",
  );
</script>

{#snippet bucketBody(bucket: CostBucket, index: number)}
  <div class="track">
    {#if bucket.visibleCostMicros > 0}
      <div class="bar" style:height={`${bucket.heightFraction * 100}%`}>
        {#each bucket.seriesCosts as slice (slice.key)}
          {#if !hiddenCost.includes(slice.key) && slice.costMicros > 0}
            <div
              class="seg"
              style:flex-grow={slice.costMicros}
              style:background={seriesColors.get(slice.key)}
            ></div>
          {/if}
        {/each}
      </div>
    {/if}
    {#if bucket.unpricedCalls > 0 && !hiddenCost.includes("unpriced")}
      <span class="unpriced-mark" title="Unpriced calls in this bucket"></span>
    {/if}
  </div>
  <div class="tick">
    {#if index % labelStep(cost.buckets.length) === 0 || index === cost.buckets.length - 1}
      <span>{bucket.label}</span>
    {/if}
  </div>
{/snippet}

<section class="charts" aria-label="Insights">
  <header class="charts-head">
    <div class="charts-title">
      <ChartColumn size={15} aria-hidden="true" />
      <h2>Insights</h2>
      <p class="scope">
        Charts for {rows.length} filtered {rows.length === 1 ? "call" : "calls"} · {scopeLabel}
      </p>
    </div>
    <button
      type="button"
      class="hide"
      aria-expanded={open}
      onclick={() => (open = !open)}
    >
      {open ? "Hide" : "Show"}
    </button>
  </header>

  {#if open}
    <div class="tabs" role="tablist" aria-label="Insight charts">
      <button
        type="button"
        role="tab"
        aria-selected={tab === "timing"}
        class:active={tab === "timing"}
        onclick={() => (tab = "timing")}
      >
        Timing
      </button>
      <button
        type="button"
        role="tab"
        aria-selected={tab === "tokens"}
        class:active={tab === "tokens"}
        onclick={() => (tab = "tokens")}
      >
        Tokens
      </button>
      <button
        type="button"
        role="tab"
        aria-selected={tab === "cost"}
        class:active={tab === "cost"}
        onclick={() => (tab = "cost")}
      >
        Cost
      </button>
    </div>

    {#if tab === "timing"}
      <div class="panel" role="tabpanel" aria-label="Timing">
        <div class="panel-head">
          <h3>Where did request time go?</h3>
          <p>
            One column per call. Compaction is part of Setup and stalls are part of
            Generation — the darker hatch inside those bars, never extra width.
          </p>
        </div>

        {#if rows.length === 0}
          <p class="empty">No model calls in this scope yet.</p>
        {:else if !timing.hasData}
          <p class="empty">
            No phase timings recorded for these calls yet. Timings appear as a call
            waits, streams, and finishes.
          </p>
        {:else}
          <div class="legend" aria-label="Timing legend">
            {#each TIMING_SEGMENTS as segment (segment.key)}
              <button
                type="button"
                class="chip"
                class:off={hiddenTiming.includes(segment.key)}
                aria-pressed={!hiddenTiming.includes(segment.key)}
                title={segment.hint}
                onclick={() => (hiddenTiming = toggle(hiddenTiming, segment.key))}
              >
                <span class="swatch" style:background={TIMING_COLORS[segment.key]}></span>
                {segment.label}
              </button>
            {/each}
          </div>

          <div class="chart" role="img" aria-label="Timing per call; values are in the table below.">
            {#each timing.columns as column, index (column.key)}
              <div class="col" title={timingTip(column)}>
                <div class="track">
                  {#if column.visibleMs > 0}
                    <div class="bar" style:height={`${column.heightFraction * 100}%`}>
                      {#each visibleTiming(column) as segment (segment.key)}
                        <div
                          class="seg"
                          style:flex-grow={segment.value}
                          style:background={TIMING_COLORS[segment.key]}
                        >
                          {#if segment.key === "setup" && column.compactionMs > 0}
                            <span
                              class="hatch"
                              style:height={`${Math.min(100, (column.compactionMs / column.setupMs) * 100)}%`}
                            ></span>
                          {:else if segment.key === "generation" && column.stallMs > 0}
                            <span
                              class="hatch stall"
                              style:height={`${Math.min(100, (column.stallMs / column.generationMs) * 100)}%`}
                            ></span>
                          {/if}
                        </div>
                      {/each}
                    </div>
                  {/if}
                </div>
                <div class="tick">
                  {#if column.status.tone !== "ok"}
                    <span class="tone {column.status.tone}" title={column.status.label}></span>
                  {/if}
                  {#if index % labelStep(timing.columns.length) === 0 || index === timing.columns.length - 1}
                    <span>{column.clock}</span>
                  {/if}
                </div>
              </div>
            {/each}
          </div>

          <details class="values">
            <summary>Values table</summary>
            <div class="table-wrap">
              <table>
                <caption>Timing per call. Compaction and stalls are subsets, not extra time.</caption>
                <thead>
                  <tr>
                    <th scope="col">Call</th>
                    <th scope="col">Setup</th>
                    <th scope="col">Compaction (in Setup)</th>
                    <th scope="col">Connect</th>
                    <th scope="col">First token</th>
                    <th scope="col">Generation</th>
                    <th scope="col">Stalls (in Generation)</th>
                    <th scope="col">Tools (after response)</th>
                    <th scope="col">Retry wait</th>
                    <th scope="col">Status</th>
                  </tr>
                </thead>
                <tbody>
                  {#each timing.columns as column (column.key)}
                    <tr>
                      <th scope="row">{column.clock}</th>
                      <td>{durationCell(column.setupMs)}</td>
                      <td>{durationCell(column.compactionMs)}</td>
                      <td>{durationCell(column.connectMs)}</td>
                      <td>{durationCell(column.firstTokenMs)}</td>
                      <td>{durationCell(column.generationMs)}</td>
                      <td>{durationCell(column.stallMs)}</td>
                      <td>{durationCell(column.toolsMs)}</td>
                      <td>{durationCell(column.retryMs)}</td>

                      <td>{column.status.label}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </details>
        {/if}
      </div>
    {:else if tab === "tokens"}
      <div class="panel" role="tabpanel" aria-label="Tokens">
        <div class="panel-head">
          <h3>What did the tokens go to?</h3>
          <p>
            Four non-overlapping segments per call: uncached input, cached input,
            visible output, and reasoning inside output. The total is input + output.
          </p>
        </div>

        {#if rows.length === 0}
          <p class="empty">No model calls in this scope yet.</p>
        {:else if tokens.summary.calls === 0}
          <p class="empty">
            No token usage reported for these calls yet. Usage appears once the
            provider sends a final usage chunk.
          </p>
        {:else}
          <p class="summary">
            {#if tokens.summary.cacheReusePct === null}
              Cache reuse not reported.
            {:else}
              Cache reuse {tokens.summary.cacheReusePct}% ·
              {formatTokenCount(tokens.summary.cachedTotal)} of
              {formatTokenCount(tokens.summary.promptTotal)} input tokens came from cache.
            {/if}
            {#if tokens.summary.missingUsage > 0}
              {tokens.summary.missingUsage} of {rows.length} calls had no reported usage.
            {/if}
          </p>

          <div class="legend" aria-label="Token legend">
            {#each TOKEN_SEGMENTS as segment (segment.key)}
              <button
                type="button"
                class="chip"
                class:off={hiddenTokens.includes(segment.key)}
                aria-pressed={!hiddenTokens.includes(segment.key)}
                title={segment.hint}
                onclick={() => (hiddenTokens = toggle(hiddenTokens, segment.key))}
              >
                <span class="swatch" style:background={TOKEN_COLORS[segment.key]}></span>
                {segment.label}
              </button>
            {/each}
          </div>

          <div class="chart" role="img" aria-label="Tokens per call; values are in the table below.">
            {#each tokens.columns as column, index (column.key)}
              <div class="col" title={tokenTip(column)}>
                <div class="track">
                  {#if column.visibleTokens > 0}
                    <div class="bar" style:height={`${column.heightFraction * 100}%`}>
                      {#each visibleTokens(column) as segment (segment.key)}
                        <div
                          class="seg"
                          style:flex-grow={segment.value}
                          style:background={TOKEN_COLORS[segment.key]}
                        ></div>
                      {/each}
                    </div>
                  {/if}
                </div>
                <div class="tick">
                  {#if !column.hasUsage}
                    <span class="tone stopped" title="No usage reported"></span>
                  {/if}
                  {#if index % labelStep(tokens.columns.length) === 0 || index === tokens.columns.length - 1}
                    <span>{column.clock}</span>
                  {/if}
                </div>
              </div>
            {/each}
          </div>

          <details class="values">
            <summary>Values table</summary>
            <div class="table-wrap">
              <table>
                <caption>Token segments per call; reasoning is inside output.</caption>
                <thead>
                  <tr>
                    <th scope="col">Call</th>
                    <th scope="col">Input</th>
                    <th scope="col">Cached input</th>
                    <th scope="col">Output</th>
                    <th scope="col">Reasoning</th>
                    <th scope="col">Cache reuse</th>
                    <th scope="col">Total</th>
                  </tr>
                </thead>
                <tbody>
                  {#each tokens.columns as column (column.key)}
                    <tr>
                      <th scope="row">{column.clock}</th>
                      <td>{column.hasUsage ? formatTokenCount(column.uncachedInput) : "—"}</td>
                      <td>{column.hasUsage ? formatTokenCount(column.cached) : "—"}</td>
                      <td>{column.hasUsage ? formatTokenCount(column.visibleOutput) : "—"}</td>
                      <td>{column.hasUsage ? formatTokenCount(column.reasoning) : "—"}</td>
                      <td>{column.cachePct === null ? "—" : `${column.cachePct}%`}</td>
                      <td>{column.hasUsage ? formatTokenCount(column.totalTokens) : "—"}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </details>
        {/if}
      </div>
    {:else}
      <div class="panel" role="tabpanel" aria-label="Cost">
        <div class="panel-head">
          <h3>What did calls cost over time?</h3>
          <p>
            {unitLabel} · UTC. Estimates use the same pricing as Cost; the Cost tab
            keeps the project's all-time ledger totals.
          </p>
        </div>

        {#if rows.length === 0}
          <p class="empty">No model calls in this scope yet.</p>
        {:else}
          <div class="cost-controls">
            <div class="grouping" role="group" aria-label="Stack cost by">
              <button
                type="button"
                class:active={grouping === "agent"}
                aria-pressed={grouping === "agent"}
                onclick={() => (grouping = "agent")}
              >
                Agent
              </button>
              <button
                type="button"
                class:active={grouping === "model"}
                aria-pressed={grouping === "model"}
                onclick={() => (grouping = "model")}
              >
                Model
              </button>
            </div>
            <p class="summary">
              {#if cost.pricedCalls > 0}
                Estimated {formatEstimate(cost.totalCostMicros)} across
                {cost.pricedCalls} priced {cost.pricedCalls === 1 ? "call" : "calls"} ·
                {formatTokenCount(cost.tokens)} tokens.
              {:else}
                No priced calls in this scope.
              {/if}
              {#if cost.unpricedCalls > 0}
                {cost.unpricedCalls}
                {cost.unpricedCalls === 1 ? "call has" : "calls have"} no price and
                {cost.unpricedCalls === 1 ? "is" : "are"} not in the estimate
                ({formatTokenCount(cost.unpricedTokens)} tokens).
              {/if}
              {#if cost.liveCalls > 0}
                {cost.liveCalls} {cost.liveCalls === 1 ? "call is" : "calls are"} still running.
              {/if}
            </p>
          </div>

          <div class="legend" aria-label="Cost legend">
            {#each cost.series as series (series.key)}
              <button
                type="button"
                class="chip"
                class:off={hiddenCost.includes(series.key)}
                aria-pressed={!hiddenCost.includes(series.key)}
                title={`Toggle ${series.label} in the cost chart`}
                onclick={() => (hiddenCost = toggle(hiddenCost, series.key))}
              >
                <span class="swatch" style:background={seriesColors.get(series.key)}></span>
                {series.label}
              </button>
            {/each}
            {#if cost.unpricedCalls > 0}
              <button
                type="button"
                class="chip"
                class:off={hiddenCost.includes("unpriced")}
                aria-pressed={!hiddenCost.includes("unpriced")}
                title="Calls with no configured price — counted, never treated as free"
                onclick={() => (hiddenCost = toggle(hiddenCost, "unpriced"))}
              >
                <span class="swatch hatch-swatch"></span>
                Unpriced
              </button>
            {/if}
          </div>

          <div class="chart cost-chart" role="group" aria-label="Estimated cost per time bucket; values are in the table below.">
            {#each cost.buckets as bucket, index (bucket.startMs)}
              {#if onbucket && bucket.calls > 0}
                <button
                  type="button"
                  class="col bucket"
                  aria-label={`Filter to ${bucketTitle(bucket)}`}
                  title={bucketTitle(bucket)}
                  onclick={() => onbucket?.(bucket.startMs, bucket.endMs)}
                >
                  {@render bucketBody(bucket, index)}
                </button>
              {:else}
                <div class="col bucket" title={bucketTitle(bucket)}>
                  {@render bucketBody(bucket, index)}
                </div>
              {/if}
            {/each}
          </div>

          <p class="footnote">
            The hatched cap marks buckets with unpriced calls. Trace covers the calls
            captured in this project; Cost covers all recorded usage.
          </p>

          <details class="values">
            <summary>Values table</summary>
            <div class="table-wrap">
              <table>
                <caption>Estimated cost per bucket (UTC). Unpriced calls are counted separately.</caption>
                <thead>
                  <tr>
                    <th scope="col">Bucket (UTC)</th>
                    <th scope="col">Calls</th>
                    <th scope="col">Tokens</th>
                    <th scope="col">Estimated cost</th>
                    {#each cost.series as series (series.key)}
                      <th scope="col">{series.label}</th>
                    {/each}
                    <th scope="col">Unpriced calls</th>
                    <th scope="col">Unpriced tokens</th>
                    <th scope="col">Still running</th>
                  </tr>
                </thead>
                <tbody>
                  {#each cost.buckets as bucket (bucket.startMs)}
                    <tr>
                      <th scope="row">{bucket.title}</th>
                      <td>{bucket.calls}</td>
                      <td>{formatTokenCount(bucket.tokens)}</td>
                      <td>{bucketCostLabel(bucket)}</td>
                      {#each cost.series as series (series.key)}
                        <td>{seriesCost(bucket, series.key)}</td>
                      {/each}
                      <td>{bucket.unpricedCalls}</td>
                      <td>{formatTokenCount(bucket.unpricedTokens)}</td>
                      <td>{bucket.liveCalls}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </details>
        {/if}
      </div>
    {/if}
  {/if}
</section>

<style>
  .charts {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--surface);
  }

  .charts-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 14px 8px;
  }
  .charts-title {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 6px;
    min-width: 0;
  }
  .charts-title :global(svg) {
    color: var(--muted);
    align-self: center;
  }
  .charts-title h2 {
    margin: 0;
    font-size: var(--text-title);
    font-weight: 620;
  }
  .scope {
    margin: 0;
    color: var(--muted);
    font-size: var(--text-meta);
  }

  .hide {
    min-height: 26px;
    padding: 3px 10px;
    border: 1px solid var(--line);
    border-radius: 7px;
    background: var(--surface);
    color: var(--ink-soft);
    cursor: pointer;
    font-size: var(--text-meta);
  }
  .hide:hover {
    border-color: var(--line-strong);
    color: var(--ink);
  }

  .tabs {
    display: flex;
    gap: 4px;
    padding: 0 14px 8px;
  }
  .tabs button {
    min-height: 26px;
    padding: 3px 12px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    color: var(--muted);
    cursor: pointer;
    font-size: var(--text-meta);
  }
  .tabs button:hover {
    border-color: var(--line-strong);
    color: var(--ink-soft);
  }
  .tabs button.active {
    border-color: var(--accent-line);
    background: var(--accent-soft);
    color: var(--accent-strong);
  }

  .panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 0 14px 14px;
    border-top: 1px solid var(--line);
    padding-top: 10px;
  }
  .panel-head h3 {
    margin: 0;
    font-size: var(--text-body);
    font-weight: 620;
  }
  .panel-head p {
    margin: 2px 0 0;
    color: var(--muted);
    font-size: var(--text-meta);
  }

  .empty {
    margin: 0;
    padding: 10px 12px;
    border: 1px dashed var(--line);
    border-radius: 8px;
    color: var(--muted);
    font-size: var(--text-body);
  }
  .summary {
    margin: 0;
    color: var(--ink-soft);
    font-size: var(--text-meta);
    font-variant-numeric: tabular-nums;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-height: 24px;
    padding: 2px 8px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink-soft);
    cursor: pointer;
    font-size: var(--text-meta);
  }
  .chip:hover {
    border-color: var(--line-strong);
  }
  .chip.off {
    color: var(--muted);
    opacity: 0.55;
  }
  .swatch {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    flex: none;
  }
  .hatch-swatch {
    border: 1px solid var(--line-strong);
    background: repeating-linear-gradient(
      45deg,
      var(--warning) 0 2px,
      transparent 2px 4px
    );
  }

  .chart {
    display: flex;
    align-items: stretch;
    gap: 2px;
    height: 132px;
    padding: 6px 2px 2px;
    overflow-x: auto;
    overflow-y: hidden;
  }
  .col {
    display: flex;
    flex: 1 1 0;
    min-width: 6px;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    min-height: 0;
  }
  .track {
    position: relative;
    display: flex;
    align-items: flex-end;
    width: 100%;
    flex: 1;
    min-height: 0;
    border-radius: 3px;
    background: var(--surface-soft);
    overflow: hidden;
  }
  .bar {
    display: flex;
    flex-direction: column-reverse;
    width: 100%;
    min-height: 1px;
    overflow: hidden;
    border-radius: 3px;
  }
  .seg {
    position: relative;
    display: block;
    width: 100%;
    flex-grow: 1;
    flex-basis: 0;
    min-height: 0;
  }
  .seg .hatch {
    position: absolute;
    inset: auto 0 0 0;
    background: repeating-linear-gradient(
      45deg,
      var(--ink) 0 1px,
      transparent 1px 3px
    );
    opacity: 0.45;
  }
  .seg .hatch.stall {
    background: repeating-linear-gradient(
      45deg,
      var(--danger) 0 1px,
      transparent 1px 3px
    );
    opacity: 0.6;
  }
  .tick {
    display: flex;
    align-items: center;
    gap: 3px;
    height: 12px;
    color: var(--muted);
    font-size: var(--text-meta);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .tone {
    width: 5px;
    height: 5px;
    border-radius: 999px;
    flex: none;
  }
  .tone.error {
    background: var(--danger);
  }
  .tone.live {
    background: var(--running-ink);
  }
  .tone.stopped {
    background: var(--muted);
  }

  .cost-controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 6px 12px;
  }
  .grouping {
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface-soft);
  }
  .grouping button {
    min-height: 22px;
    padding: 2px 10px;
    border: 0;
    border-radius: 999px;
    background: transparent;
    color: var(--muted);
    cursor: pointer;
    font-size: var(--text-meta);
  }
  .grouping button.active {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow-chip);
  }

  .cost-chart {
    height: 120px;
  }
  .bucket {
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    cursor: default;
  }
  button.bucket {
    cursor: pointer;
  }
  button.bucket:hover .track,
  button.bucket:focus-visible .track {
    outline: 1px solid var(--accent-line);
    outline-offset: -1px;
  }
  button.bucket:focus-visible {
    outline: none;
  }
  .unpriced-mark {
    position: absolute;
    top: 2px;
    left: 0;
    right: 0;
    height: 3px;
    background: repeating-linear-gradient(
      45deg,
      var(--warning) 0 2px,
      transparent 2px 4px
    );
  }

  .footnote {
    margin: 0;
    color: var(--muted);
    font-size: var(--text-meta);
  }

  .values {
    border-top: 1px solid var(--line);
    padding-top: 6px;
  }
  .values summary {
    color: var(--muted);
    cursor: pointer;
    font-size: var(--text-meta);
  }
  .table-wrap {
    max-height: 260px;
    overflow: auto;
    margin-top: 6px;
  }
  table {
    border-collapse: collapse;
    width: 100%;
    font-size: var(--text-meta);
    font-variant-numeric: tabular-nums;
  }
  caption {
    padding-bottom: 4px;
    color: var(--muted);
    font-size: var(--text-meta);
    text-align: left;
  }
  th,
  td {
    padding: 4px 8px;
    border-bottom: 1px solid var(--line);
    text-align: right;
    white-space: nowrap;
  }
  th:first-child,
  td:first-child {
    text-align: left;
  }
  thead th {
    color: var(--muted);
    font-weight: 600;
    position: sticky;
    top: 0;
    background: var(--surface);
  }
</style>
