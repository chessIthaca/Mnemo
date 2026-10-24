<script lang="ts">
  import { ArrowUpRight, Check, GitMerge } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import type { LeadEntry } from "../lib/types";

  let { entry }: { entry: Extract<LeadEntry, { kind: "merge" }> } = $props();
  const worker = $derived($app.workers.find((item) => item.id === entry.workerId));
</script>

<article class="merge-card">
  <div class="merge-topline">
    <span class="merge-icon"><GitMerge size={16} /></span>
    <span class="merge-label">MERGE RECORD</span>
    <time>{entry.time}</time>
  </div>
  <h3>{worker?.name ?? "Worker"}’s branch is in <code>{entry.record.target}</code></h3>
  <p class="merge-commit"><Check size={13} /> <code>{entry.record.commit}</code> · {entry.record.branch}</p>
  <div class="merge-stats">
    <span><strong>{entry.record.files}</strong> files</span>
    <span class="additions">+{entry.record.additions}</span>
    <span class="deletions">−{entry.record.deletions}</span>
    <span>{entry.record.reviewSummary}</span>
  </div>
  <div class="merge-footer">
    <span>{entry.record.checks.length} check passed · {entry.record.model} · ${entry.record.cost.toFixed(2)}</span>
    <button onclick={() => actions.selectWorker(entry.workerId)}>View worker <ArrowUpRight size={13} /></button>
  </div>
</article>

<style>
  .merge-card { margin: 18px 0 18px 34px; padding: 17px 18px; border: 1px solid #d8e0d2; border-radius: 16px; background: #f4f7f1; }
  .merge-topline { display: flex; align-items: center; gap: 8px; color: var(--accent-strong); }
  .merge-icon { display: grid; width: 27px; height: 27px; place-items: center; border-radius: 9px; background: #e2ebdc; }
  .merge-label { font-size: 8px; font-weight: 750; letter-spacing: 0.12em; }
  time { margin-left: auto; color: var(--muted); font-size: 9px; }
  h3 { margin: 12px 0 6px; font-family: var(--font-prose); font-size: 17px; font-weight: 550; }
  h3 code { padding: 2px 5px; border-radius: 5px; background: #e5ecdf; font-family: var(--font-mono); font-size: 11px; }
  .merge-commit { display: flex; align-items: center; gap: 5px; margin: 0; color: var(--muted); font-size: 10px; }
  .merge-commit code { color: var(--accent-strong); font-family: var(--font-mono); }
  .merge-stats { display: flex; flex-wrap: wrap; gap: 12px; margin: 15px 0; color: var(--muted); font-size: 10px; }
  .merge-stats strong { color: var(--ink); font-weight: 600; }
  .additions { color: #4f7d48; }
  .deletions { color: #a15b53; }
  .merge-footer { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding-top: 12px; border-top: 1px solid #dfe6d9; color: var(--muted); font-size: 9px; }
  .merge-footer button { display: inline-flex; align-items: center; gap: 4px; padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: 10px; font-weight: 650; }
  @media (max-width: 560px) { .merge-card { margin-left: 0; } .merge-footer { align-items: flex-start; flex-direction: column; } }
</style>
