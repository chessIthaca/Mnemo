<script lang="ts">
  import { ArrowUpRight, Check, GitMerge } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import type { LeadEntry } from "../lib/types";
  import EventRow from "./EventRow.svelte";

  let { entry }: { entry: Extract<LeadEntry, { kind: "merge" }> } = $props();
  const worker = $derived($app.workers.find((item) => item.id === entry.workerId));
  const name = $derived(worker?.name ?? entry.workerName ?? entry.workerId);
</script>

<EventRow tone="merge" label={`${name}’s work landed in ${entry.record.target}`}>
  {#snippet icon()}<GitMerge size={13} />{/snippet}
  {#snippet heading()}{name}’s work landed in <code>{entry.record.target}</code>{#if entry.record.revertCommit}<span class="reverted">{" · reverted"}</span>{/if}{/snippet}
  <p class="merge-commit"><Check size={12} /> <code>{entry.record.commit}</code> · {entry.record.branch} · {entry.time}</p>
  <div class="merge-stats">
    <span><strong>{entry.record.files}</strong> files</span>
    <span class="additions">+{entry.record.additions}</span>
    <span class="deletions">−{entry.record.deletions}</span>
    {#if entry.record.reviewSummary}<span>{entry.record.reviewSummary}</span>{/if}
  </div>
  <p class="merge-meta">
    {entry.record.checks.length} check{entry.record.checks.length === 1 ? "" : "s"} passed · {entry.record.model} · {entry.record.tokens} tokens{#if entry.record.cost != null} · ${entry.record.cost.toFixed(2)}{/if}
  </p>
  <div class="merge-actions">
    <button onclick={() => actions.selectWorker(entry.workerId)}>View worker <ArrowUpRight size={13} /></button>
    {#if entry.record.revertCommit}
      <span>Reverted · <code>{entry.record.revertCommit}</code></span>
    {:else}
      <button aria-label={`Revert merge for ${name}`} onclick={() => actions.revertMerge(entry.workerId)}>Revert</button>
    {/if}
  </div>
</EventRow>

<style>
  code { font-family: var(--font-mono); font-size: var(--text-sm); }
  .reverted { color: var(--muted); font-weight: 500; }
  .merge-commit { display: flex; align-items: center; gap: 5px; margin: 0; color: var(--muted); font-size: var(--text-sm); }
  .merge-commit code { color: var(--accent-strong); }
  .merge-stats { display: flex; flex-wrap: wrap; gap: 12px; margin: 8px 0 4px; color: var(--muted); font-size: var(--text-sm); }
  .merge-stats strong { color: var(--ink); font-weight: 600; }
  .additions { color: #4f7d48; }
  .deletions { color: #a15b53; }
  .merge-meta { margin: 0; color: var(--muted); font-size: var(--text-xs); }
  .merge-actions { display: flex; align-items: center; gap: 12px; margin-top: 8px; color: var(--muted); font-size: var(--text-xs); }
  .merge-actions button { display: inline-flex; align-items: center; gap: 4px; padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: var(--text-sm); font-weight: 650; }
</style>
