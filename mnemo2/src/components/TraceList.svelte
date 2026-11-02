<script lang="ts">
  // Copyright (c) 2026 Carsten Hess
  // SPDX-License-Identifier: MIT
  // See LICENSE in the repository root.

  import { Archive, CircleAlert, TriangleAlert } from "@lucide/svelte";
  import {
    actorLabel,
    actorSubLabel,
    callStatus,
    cacheReuseLabel,
    formatDuration,
    formatTokens,
    phaseTotalMs,
    purposeLabel,
    rowTokenSummary,
    traceAge,
    traceClock,
    traceCostLabel,
    traceKey,
    type TraceRow,
    type TraceRowGroup,
  } from "../lib/trace";

  /**
   * The list column: grouped calls newest-first, every row readable at a
   * glance — who, purpose, model, status, timing, tokens and cost — with the
   * follow pill that never moves content under the pointer.
   */
  let {
    groups,
    selected,
    newCount,
    hasMore,
    loadingMore,
    nowMs,
    onselect,
    onloadmore,
    onfollow,
  }: {
    groups: TraceRowGroup[];
    /** The durable key of the open call. */
    selected: string | null;
    /** Calls that arrived while following was off. */
    newCount: number;
    hasMore: boolean;
    loadingMore: boolean;
    nowMs: number;
    onselect: (row: TraceRow) => void;
    onloadmore: () => void;
    onfollow: () => void;
  } = $props();

  const rows = $derived(groups.flatMap((group) => group.rows));
</script>

<div class="trace-list">
  {#if newCount > 0}
    <div class="follow-row">
      <button class="follow-pill" type="button" onclick={onfollow}>
        {newCount} new call{newCount === 1 ? "" : "s"} — Follow
      </button>
    </div>
  {/if}

  {#each groups as group (group.key)}
    <section class="group">
      <header class="group-head">
        <h4>{group.title}</h4>
        <p>{group.meta}</p>
      </header>
      <ul class="rows">
        {#each group.rows as row (traceKey(row))}
          {@const status = callStatus(row)}
          {@const tokens = rowTokenSummary(row)}
          {@const subLabel = actorSubLabel(row.attribution)}
          <li>
            <button
              type="button"
              class="row"
              class:open={selected === traceKey(row)}
              aria-current={selected === traceKey(row) ? "true" : undefined}
              title={`${row.model} via ${row.provider}`}
              onclick={() => onselect(row)}
            >
              <span class="row-top">
                <span class="dot {status.tone}" aria-hidden="true"></span>
                <span class="actor">{actorLabel(row.attribution)}</span>
                {#if subLabel !== ""}<span class="sub">{subLabel}</span>{/if}
                <span class="spacer"></span>
                {#if row.retained}
                  <Archive size={12} class="warn" aria-label="Retained metadata — bodies not kept" title="Retained metadata — the prompt and response were not kept" />
                {/if}
                {#if row.response_truncated || row.response_evicted}
                  <TriangleAlert size={12} class="warn" title="The recorded response was cut short in the log" aria-label="Response truncated in the log" />
                {/if}
                {#if row.guard_cut !== null}
                  <TriangleAlert size={12} class="warn" title={`A client-side stream guard ended this call (${row.guard_cut.boundary})`} aria-label="Guard cut" />
                {/if}
                {#if row.error !== null}
                  <CircleAlert size={12} class="warn danger" title="This call failed — open it for the error text" aria-label="Failed" />
                {/if}
                <span class="status {status.tone}">{status.label}</span>
              </span>
              <span class="row-line">
                {row.attribution === null ? "Unknown purpose" : purposeLabel(row.attribution.purpose)} · {row.model}
              </span>
              <span class="row-line mono">
                {traceClock(row.ts_ms)}
                <span title={traceAge(row.ts_ms, nowMs)}>{traceAge(row.ts_ms, nowMs)}</span>
                · {cacheReuseLabel(row.usage)}
                · {formatDuration(phaseTotalMs(row))}
                · {formatTokens(tokens.input ?? 0)} in · {formatTokens(tokens.output ?? 0)} out
                · {traceCostLabel(row.cost_micros)}
              </span>
            </button>
          </li>
        {/each}
      </ul>
    </section>
  {/each}

  {#if hasMore}
    <button class="load-more" type="button" disabled={loadingMore} onclick={onloadmore}>
      {loadingMore ? "Loading…" : "Load more"}
    </button>
  {/if}
</div>

<style>
  .trace-list { display: grid; gap: 4px; }
  .follow-row { position: sticky; top: 0; z-index: 5; display: grid; padding: 6px 0; background: linear-gradient(var(--canvas) 60%, transparent); }
  .follow-pill { justify-self: center; display: inline-flex; align-items: center; gap: 6px; padding: 5px 12px; border: 1px solid var(--accent); border-radius: 999px; background: var(--surface); color: var(--accent-strong); cursor: pointer; font-size: var(--text-meta); font-weight: 650; box-shadow: var(--shadow); }
  .follow-pill:hover { border-color: var(--accent-strong); }
  .group { display: grid; gap: 2px; }
  .group-head { display: flex; align-items: baseline; justify-content: space-between; gap: 10px; padding: 10px 12px 3px; }
  .group-head h4 { margin: 0; color: var(--ink-soft); font-size: var(--text-body); font-weight: 650; }
  .group-head p { margin: 0; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); font-variant-numeric: tabular-nums; }
  .rows { display: grid; gap: 2px; margin: 0; padding: 0; list-style: none; }
  .row { display: grid; width: 100%; gap: 3px; padding: 9px 12px; border: 0; border-radius: 10px; background: transparent; cursor: pointer; text-align: left; }
  .row:hover { background: var(--surface-soft); }
  .row.open { background: var(--surface); box-shadow: inset 0 0 0 1px var(--accent); }
  .row:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .row-top { display: flex; align-items: center; gap: 7px; color: var(--ink); font-size: var(--text-body); font-weight: 650; }
  .dot { flex: 0 0 auto; width: 8px; height: 8px; border-radius: 999px; }
  .dot.ok { background: var(--stat-add); }
  .dot.live { background: var(--running-ink); }
  .dot.error { background: var(--danger); }
  .dot.stopped { background: var(--muted); }
  .dot.guard { background: var(--warning); }
  .actor { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sub { overflow: hidden; color: var(--muted); font-size: var(--text-meta); font-weight: 500; text-overflow: ellipsis; white-space: nowrap; }
  .spacer { flex: 1; }
  .warn { color: var(--warning); }
  .warn.danger { color: var(--danger); }
  .status { flex: 0 0 auto; padding: 2px 8px; border-radius: 999px; font-size: var(--text-meta); font-weight: 650; }
  .status.ok { background: var(--stat-add-bg); color: var(--stat-add); }
  .status.live { background: var(--running-bg); color: var(--running-ink); }
  .status.error { background: var(--danger-bg); color: var(--danger); }
  .status.stopped { background: var(--surface-soft); color: var(--muted); }
  .status.guard { background: var(--warning-soft); color: var(--warning); }
  .row-line { display: flex; gap: 6px; color: var(--muted); font-size: var(--text-meta); line-height: 1.45; }
  .row-line.mono { color: var(--ink-soft); font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .load-more { justify-self: stretch; margin: 8px 12px; padding: 7px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: var(--text-meta); font-weight: 600; }
  .load-more:hover:not(:disabled) { border-color: var(--accent); color: var(--accent-strong); }
  .load-more:disabled { opacity: 0.6; cursor: default; }
</style>