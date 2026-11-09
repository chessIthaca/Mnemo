<script lang="ts">
  import { ChartColumn, CircleAlert } from "@lucide/svelte";
  import {
    costPreviewInsights,
    costViewModel,
    createCostTransport,
    type CostReport,
    type SavingsReport,
  } from "../lib/cost";

  /** How often an open tab re-reads the reports. */
  const REFRESH_MS = 30_000;

  const transport = createCostTransport();
  // Outside the desktop host (the browser preview page) the seeded sample
  // serves instead, so `preview.html?scene=cost` shows the real layout.
  const seeded = transport === null ? costPreviewInsights() : null;

  let report = $state<CostReport | null>(seeded?.cost ?? null);
  let savings = $state<SavingsReport | null>(seeded?.savings ?? null);
  let error = $state("");

  /** Turn a rejected command into one readable line (errors stay inline). */
  function errorMessage(value: unknown): string {
    if (typeof value === "string") return value;
    return value instanceof Error ? value.message : "Reading the cost reports failed.";
  }

  async function load(): Promise<void> {
    if (!transport) return;
    try {
      const [costReport, savingsReport] = await Promise.all([transport.cost(), transport.savings()]);
      report = costReport;
      savings = savingsReport;
      error = "";
    } catch (failure) {
      error = errorMessage(failure);
    }
  }

  // The tab mounts only while its route is active, so this effect is the
  // tab-open refresh plus the 30 s poll; its cleanup clears the timer the
  // moment the route moves away.
  $effect(() => {
    if (!transport) return;
    void load();
    const timer = setInterval(() => void load(), REFRESH_MS);
    return () => clearInterval(timer);
  });

  const view = $derived(report !== null && savings !== null ? costViewModel(report, savings) : null);
</script>

<section class="insight-view" aria-label="Cost">
  <header class="insight-header">
    <h1>Cost</h1>
    <p>Project spend, savings and prompt-cache hits, by model and by day.</p>
  </header>

  {#if transport === null && view === null}
    <div class="insight-placeholder">
      <ChartColumn size={22} />
      <p>Cost insights are available in the desktop app.</p>
    </div>
  {:else if error !== "" && view === null}
    <div class="insight-placeholder">
      <CircleAlert size={22} />
      <p role="alert">{error}</p>
      <button class="retry" onclick={() => void load()}>Try again</button>
    </div>
  {:else if view === null}
    <div class="insight-placeholder">
      <p>Reading the project ledger …</p>
    </div>
  {:else if view.empty}
    <div class="insight-placeholder">
      <ChartColumn size={22} />
      <p>Nothing recorded yet. Spend and savings appear once the lead or a worker runs a turn.</p>
    </div>
  {:else}
    <div class="cost-scroll">
      {#if error !== ""}<p class="refresh-error" role="status">{error}</p>{/if}

      <section class="cost-card headline" aria-label="Project spend">
        <p class="card-label">Project spend</p>
        <p class="spend">{view.spendLabel}</p>
        <p class="headline-tokens">{view.tokensLabel}</p>
        {#if view.partial}
          <p class="partial">Partial estimate — no price for {view.unpricedLabel}.</p>
        {/if}
      </section>

      {#if view.models.length > 0}
        <section class="cost-card" aria-label="Spend by model">
          <h2>Spend by model</h2>
          <ul class="model-list">
            {#each view.models as row (row.id)}
              <li class="model-row" title={row.detailLabel}>
                <p class="model-name">{row.label}</p>
                <div class="model-bar" aria-hidden="true">
                  <span class="model-bar-fill" style:width={`${Math.round(row.barFraction * 100)}%`}></span>
                </div>
                <p class="model-tokens">{row.tokensLabel}</p>
                <p class="model-cost" class:unpriced={row.costMicros === null}>{row.costLabel}</p>
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      <section class="cost-card" aria-label="Savings">
        <div class="card-head">
          <h2>Savings</h2>
          <p class="card-total">{view.savingsTokensLabel} tokens saved · {view.savingsCostLabel}</p>
        </div>
        {#if view.savingsBasisLabel !== ""}
          <p class="savings-note">{view.savingsBasisLabel}</p>
        {/if}
        <p class="savings-note">{view.savingsMethodLabel}</p>
        {#if view.levers.length === 0}
          <p class="quiet">No savings recorded yet.</p>
        {:else}
          <ul class="lever-list">
            {#each view.levers as lever (lever.kind)}
              <li class="lever-row">
                <div class="lever-text">
                  <p class="lever-name">{lever.label}</p>
                  <p class="lever-explain">{lever.explanation}</p>
                </div>
                <div class="lever-numbers">
                  <p class="lever-tokens" class:readded={lever.readded}>{lever.metaLabel}</p>
                  {#if lever.costLabel !== ""}<p class="lever-cost">{lever.costLabel}</p>{/if}
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      <section class="cost-card" aria-label="Prompt cache">
        <h2>Prompt cache</h2>
        {#if view.cacheActive}
          <div class="cache-row">
            <p class="cache-hit">{view.cacheHitRateLabel}</p>
            <div class="cache-text">
              <p class="cache-saved">{view.cacheSavedLabel} saved</p>
              <p class="cache-detail">{view.cacheDetailLabel}</p>
            </div>
          </div>
        {:else}
          <p class="quiet">No prompt tokens recorded yet.</p>
        {/if}
      </section>

      <section class="cost-card" aria-label="Last 14 days">
        <h2>Last 14 days</h2>
        {#if view.days.length === 0 || (!view.daysHaveSpend && !view.daysHaveSavings)}
          <p class="quiet">No per-day activity yet.</p>
        {:else}
          {#if view.daysHaveSpend}
            <div class="day-block">
              <p class="day-label">Spend per day · estimated</p>
              <div class="day-chart">
                {#each view.days as day (day.day)}
                  <div class="day-col" title="{day.label} · {day.costLabel}">
                    <div class="day-bar-area">
                      <div class="day-bar spend" style:height={`${Math.round(day.costFraction * 100)}%`}></div>
                    </div>
                    <p class="day-name">{day.shortLabel}</p>
                  </div>
                {/each}
              </div>
            </div>
          {/if}
          {#if view.daysHaveSavings}
            <div class="day-block">
              <p class="day-label">Tokens saved per day</p>
              <div class="day-chart">
                {#each view.days as day (day.day)}
                  <div class="day-col" title="{day.label} · {day.tokensSavedLabel} tokens">
                    <div class="day-bar-area">
                      <div class="day-bar saved" style:height={`${Math.round(day.tokensFraction * 100)}%`}></div>
                    </div>
                    <p class="day-name">{day.shortLabel}</p>
                  </div>
                {/each}
              </div>
            </div>
          {/if}
        {/if}
      </section>
    </div>
  {/if}
</section>

<style>
  .insight-view { display: flex; flex: 1; min-width: 0; min-height: 0; flex-direction: column; background: var(--canvas); }
  .insight-header { padding: 18px 24px 12px; }
  .insight-header h1 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 500; letter-spacing: -0.03em; }
  .insight-header p { margin: 6px 0 0; color: var(--muted); font-size: var(--text-body); }
  .insight-placeholder { display: grid; flex: 1; place-items: center; align-content: center; gap: 10px; padding: 40px 24px; border-top: 1px solid var(--line); color: var(--muted); text-align: center; }
  .insight-placeholder p { margin: 0; font-size: var(--text-body); }

  .retry {
    min-height: 30px;
    padding: 5px 12px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--surface);
    color: var(--ink-soft);
    cursor: pointer;
    font-size: var(--text-body);
  }
  .retry:hover { border-color: var(--line-strong); color: var(--ink); }

  .cost-scroll {
    display: grid;
    flex: 1;
    gap: 12px;
    align-content: start;
    min-height: 0;
    overflow-y: auto;
    padding: 12px 24px 24px;
    border-top: 1px solid var(--line);
  }

  .refresh-error {
    margin: 0;
    padding: 8px 12px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--surface);
    color: var(--danger);
    font-size: var(--text-body);
  }

  .cost-card {
    padding: 14px 16px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--surface);
  }
  .cost-card h2 { margin: 0 0 10px; font-size: var(--text-title); font-weight: 620; }

  .headline .card-label {
    margin: 0;
    color: var(--muted);
    font-size: var(--text-meta);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .headline .spend {
    margin: 6px 0 0;
    font-family: var(--font-prose);
    font-size: var(--text-page);
    font-weight: 500;
    letter-spacing: -0.02em;
  }
  .headline-tokens { margin: 6px 0 0; color: var(--ink-soft); font-size: var(--text-body); font-variant-numeric: tabular-nums; }
  .partial { margin: 8px 0 0; color: var(--warning); font-size: var(--text-meta); }

  .model-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
  .model-row {
    display: grid;
    grid-template-columns: minmax(96px, 1.1fr) 1.6fr auto auto;
    gap: 10px;
    align-items: center;
  }
  .model-row p { margin: 0; }
  .model-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: var(--text-body); font-weight: 620; }
  .model-bar { height: 4px; border-radius: 999px; background: var(--accent-soft); overflow: hidden; }
  .model-bar-fill { display: block; height: 100%; border-radius: inherit; background: var(--accent); }
  .model-tokens { color: var(--ink-soft); font-size: var(--text-body); font-variant-numeric: tabular-nums; }
  .model-cost { min-width: 64px; text-align: right; font-size: var(--text-body); font-variant-numeric: tabular-nums; }
  .model-cost.unpriced { color: var(--muted); font-style: italic; }

  .card-head { display: flex; align-items: baseline; justify-content: space-between; gap: 10px; margin-bottom: 10px; }
  .card-head h2 { margin: 0; }
  .card-total { margin: 0; color: var(--ink-soft); font-size: var(--text-body); font-variant-numeric: tabular-nums; }
.savings-note { margin: 2px 0 0; color: var(--muted); font-size: var(--text-meta); line-height: 1.45; }

  .lever-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 10px; }
  .lever-row { display: grid; grid-template-columns: 1fr auto; gap: 12px; align-items: start; }
  .lever-text p { margin: 0; }
  .lever-name { font-size: var(--text-body); font-weight: 620; }
  .lever-explain { margin-top: 2px; color: var(--muted); font-size: var(--text-body); }
  .lever-numbers { display: grid; gap: 2px; justify-items: end; text-align: right; }
  .lever-numbers p { margin: 0; font-size: var(--text-body); font-variant-numeric: tabular-nums; }
  .lever-tokens { color: var(--ink-soft); }
  .lever-tokens.readded { color: var(--muted); }

  .cache-row { display: flex; align-items: center; gap: 14px; }
  .cache-hit { margin: 0; font-family: var(--font-prose); font-size: var(--text-title); font-weight: 620; }
  .cache-text p { margin: 0; }
  .cache-saved { font-size: var(--text-body); font-variant-numeric: tabular-nums; }
  .cache-detail { margin-top: 2px; color: var(--muted); font-size: var(--text-meta); }

  .day-block + .day-block { margin-top: 12px; }
  .day-label { margin: 0 0 6px; color: var(--muted); font-size: var(--text-meta); }
  .day-chart { display: flex; gap: 4px; align-items: stretch; }
  .day-col { display: flex; flex: 1; min-width: 0; flex-direction: column; gap: 4px; align-items: center; }
  .day-bar-area { position: relative; width: 100%; height: 34px; border-radius: 4px; background: var(--surface-soft); overflow: hidden; }
  .day-bar { position: absolute; inset: auto 0 0 0; border-radius: inherit; }
  .day-bar.spend { background: var(--accent); }
  .day-bar.saved { background: var(--ink-soft); }
  .day-name { margin: 0; max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--muted); font-size: var(--text-meta); }

  .quiet { margin: 0; color: var(--muted); font-size: var(--text-body); }
</style>
