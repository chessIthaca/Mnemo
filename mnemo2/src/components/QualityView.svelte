<script lang="ts">
  // Copyright (c) 2026 Carsten Hess
  // SPDX-License-Identifier: MIT
  // See LICENSE in the repository root.

  /**
   * The Quality tab: how the merged work over a window held up, per final tier
   * and per final model — how often it landed first time, how much fixing it
   * needed, whether it was reverted, and whether a bug-fix plan touched the
   * same files soon after. Read-only; the numbers come from the routing log
   * through the one `quality_report` command.
   */
  import { ChartColumn, CircleAlert } from "@lucide/svelte";
  import {
    createQualityTransport,
    DEFAULT_QUALITY_WINDOW,
    formatMean,
    formatShare,
    QUALITY_WINDOWS,
    type QualityReport,
    type QualityRow,
  } from "../lib/quality";
  import { formatUsageCost } from "../lib/usage";

  // Absent outside the desktop host (and whenever a test plants none).
  const transport = createQualityTransport();

  let windowDays = $state(DEFAULT_QUALITY_WINDOW);
  let report = $state<QualityReport | null>(null);
  let error = $state("");
  /** The newest request; a reply to an older one is dropped, so a fast window change cannot overwrite the new window's report. */
  let pending = 0;

  /** Turn a rejected command into one readable line (errors stay inline). */
  function errorMessage(value: unknown): string {
    if (typeof value === "string") return value;
    return value instanceof Error ? value.message : "Reading the routing log failed.";
  }

  async function load(days: number): Promise<void> {
    if (!transport) return;
    const ticket = ++pending;
    try {
      const next = await transport.report(days);
      if (ticket !== pending) return;
      report = next;
      error = "";
    } catch (failure) {
      if (ticket !== pending) return;
      error = errorMessage(failure);
    }
  }

  // The tab mounts only while it is open, so this effect is the refresh on
  // open; the window selector re-runs it with the new window.
  $effect(() => {
    const days = windowDays;
    if (!transport) return;
    void load(days);
  });

  /** No merges at all: the window has nothing to judge yet. */
  const empty = $derived(
    report !== null && report.byTier.length === 0 && report.byModel.length === 0,
  );

  /** The measuring columns both tables share, in order. */
  const columns = [
    "Merged",
    "First pass",
    "Fix rounds",
    "Findings",
    "Reverted",
    "Bug fix followed",
    "Cost / merged",
  ] as const;
</script>

{#snippet tableRows(rows: QualityRow[])}
  {#each rows as row (row.key)}
    <tr>
      <th scope="row">{row.key}</th>
      <td>{row.merged}</td>
      <td>{formatShare(row.firstPass)}</td>
      <td>{formatMean(row.avgFixRounds)}</td>
      <td>{formatMean(row.avgFindings)}</td>
      <td>{row.reverted}</td>
      <td>{row.bugfixFollowups}</td>
      <td>{formatUsageCost(row.costPerMergedMicros)}</td>
    </tr>
  {/each}
{/snippet}

{#snippet table(rows: QualityRow[], title: string, first: string)}
  <section class="card">
    <h2>{title}</h2>
    <div class="table-wrap">
      <table aria-label={title}>
        <thead>
          <tr>
            <th scope="col">{first}</th>
            {#each columns as column (column)}
              <th scope="col">{column}</th>
            {/each}
          </tr>
        </thead>
        <tbody>
          {@render tableRows(rows)}
        </tbody>
      </table>
    </div>
  </section>
{/snippet}
<section class="quality-view" aria-label="Quality">
  <header class="quality-header">
    <div>
      <h1>Quality</h1>
      <p>How merged work held up — first-pass rate, fixing, reverts and later bug fixes — by tier and by model.</p>
    </div>
    <label class="window">
      <span>Window</span>
      <select
        aria-label="Window"
        value={String(windowDays)}
        onchange={(event) => { windowDays = Number(event.currentTarget.value); }}
      >
        {#each QUALITY_WINDOWS as days (days)}
          <option value={String(days)}>{days} days</option>
        {/each}
      </select>
    </label>
  </header>

  {#if transport === null}
    <div class="placeholder">
      <ChartColumn size={22} />
      <p>The Quality tab is available in the desktop app.</p>
    </div>
  {:else if report === null}
    <div class="placeholder">
      {#if error !== ""}
        <CircleAlert size={22} />
        <p role="alert">{error}</p>
        <button class="retry" onclick={() => void load(windowDays)}>Try again</button>
      {:else}
        <p>Reading the routing log …</p>
      {/if}
    </div>
  {:else}
    <div class="quality-scroll">
      {#if error !== ""}
        <p class="refresh-error" role="alert">{error}</p>
      {/if}

      {#if empty}
        <p class="empty">No merged work in this window yet.</p>
      {:else}
        {@render table(report.byTier, "By tier", "Tier")}
        {@render table(report.byModel, "By model", "Model")}

        {#if report.categories.length > 0}
          <section class="card" aria-label="Recurring findings">
            <h2>Recurring findings</h2>
            <ul class="categories">
              {#each report.categories as [category, workers] (category)}
                <li>
                  <span class="category">{category}</span>
                  <span
                    class="category-count"
                    title="Distinct workers whose reviews logged this category"
                  >{workers}</span>
                </li>
              {/each}
            </ul>
          </section>
        {/if}
      {/if}

      {#if report.missingMergeCommit > 0}
        <p class="footnote">
          {report.missingMergeCommit} older tasks have no merge commit recorded, so they are not checked for follow-up bug fixes.
        </p>
      {/if}
    </div>
  {/if}
</section>

<style>
  .quality-view { display: flex; flex: 1; min-width: 0; min-height: 0; flex-direction: column; background: var(--canvas); }
  .quality-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; padding: 18px 24px 12px; }
  .quality-header h1 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 500; letter-spacing: -0.03em; }
  .quality-header p { margin: 6px 0 0; color: var(--muted); font-size: var(--text-body); }
  .window { display: inline-flex; flex: 0 0 auto; align-items: center; gap: 8px; color: var(--muted); font-size: var(--text-meta); }
  .window select { padding: 5px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink); cursor: pointer; font-size: var(--text-meta); }
  .window select:hover { border-color: var(--line-strong); }
  .window select:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }

  .placeholder { display: grid; flex: 1; place-items: center; align-content: center; gap: 10px; padding: 40px 24px; border-top: 1px solid var(--line); color: var(--muted); text-align: center; }
  .placeholder p { margin: 0; font-size: var(--text-body); }
  .retry { min-height: 30px; padding: 5px 12px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: var(--text-body); }
  .retry:hover { border-color: var(--line-strong); color: var(--ink); }

  .quality-scroll { display: grid; flex: 1; gap: 12px; align-content: start; min-height: 0; overflow-y: auto; padding: 12px 24px 24px; border-top: 1px solid var(--line); }
  .empty { margin: 0; color: var(--muted); font-size: var(--text-body); }
  .refresh-error { margin: 0; padding: 8px 12px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--danger); font-size: var(--text-body); }
  .footnote { margin: 0; color: var(--muted); font-size: var(--text-meta); }

  .card { padding: 14px 16px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
  .card h2 { margin: 0 0 8px; font-size: var(--text-title); font-weight: 620; }
  .table-wrap { max-height: 320px; overflow: auto; }
  table { border-collapse: collapse; width: 100%; font-size: var(--text-meta); font-variant-numeric: tabular-nums; }
  th,
  td { padding: 4px 8px; border-bottom: 1px solid var(--line); text-align: right; white-space: nowrap; }
  th:first-child,
  td:first-child { text-align: left; }
  thead th { color: var(--muted); font-weight: 600; }
  tbody th { font-weight: 500; }

  .categories { display: grid; gap: 2px; margin: 0; padding: 0; list-style: none; font-size: var(--text-meta); }
  .categories li { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; padding: 3px 0; border-bottom: 1px solid var(--line); }
  .category { color: var(--ink-soft); overflow-wrap: anywhere; }
  .category-count { flex: 0 0 auto; color: var(--muted); font-variant-numeric: tabular-nums; }
</style>
