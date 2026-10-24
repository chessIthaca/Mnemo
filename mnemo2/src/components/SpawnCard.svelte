<script lang="ts">
  import { ArrowRight, UsersRound } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import type { LeadEntry } from "../lib/types";
  import ModelBadge from "./ModelBadge.svelte";

  let { entry }: { entry: Extract<LeadEntry, { kind: "spawn" }> } = $props();
  const workers = $derived($app.workers.filter((worker) => entry.workerIds.includes(worker.id)));
</script>

<article class="spawn-card">
  <div class="card-heading">
    <span class="card-icon"><UsersRound size={16} /></span>
    <div><p class="eyebrow">DELEGATION</p><h3>{entry.title}</h3></div>
    <time>{entry.time}</time>
  </div>
  <p class="spawn-copy">{entry.body}</p>
  <div class="spawn-workers">
    {#each workers.slice(0, 5) as worker (worker.id)}
      <button class="worker-chip" onclick={() => actions.selectWorker(worker.id)} title={worker.task}>
        <span class="avatar" style="--hue: {worker.hue}">{worker.initials}</span>
        <span class="worker-copy"><strong>{worker.name}</strong><ModelBadge model={worker.model} /></span>
        <span class="status-dot" data-status={worker.status}></span>
      </button>
    {/each}
    {#if workers.length > 5}<span class="more-workers">+{workers.length - 5} more</span>{/if}
  </div>
  <button class="card-action" onclick={actions.openTeam}>Open team <ArrowRight size={14} /></button>
</article>

<style>
  .spawn-card { margin: 14px 0 14px 30px; padding: 15px; border: 1px solid var(--line); border-radius: 16px; background: var(--surface); box-shadow: var(--shadow-soft); }
  .card-heading { display: flex; align-items: center; gap: 11px; }
  .card-icon { display: grid; width: 32px; height: 32px; place-items: center; border-radius: 10px; background: var(--accent-soft); color: var(--accent-strong); }
  .eyebrow { margin: 0 0 3px; color: var(--muted); font-size: 8px; font-weight: 700; letter-spacing: 0.12em; }
  h3 { margin: 0; font-size: 13px; font-weight: 650; }
  time { margin-left: auto; color: var(--muted); font-size: 9px; }
  .spawn-copy { margin: 12px 0 14px; color: var(--muted); font-size: 11px; line-height: 1.6; }
  .spawn-workers { display: flex; flex-wrap: wrap; gap: 7px; }
  .worker-chip { display: flex; align-items: center; gap: 8px; min-width: 165px; padding: 7px 9px; border: 1px solid var(--line); border-radius: 10px; background: var(--canvas); color: var(--ink); cursor: pointer; text-align: left; }
  .worker-chip:hover { border-color: var(--line-strong); background: var(--surface-soft); }
  .worker-copy { min-width: 0; }
  .worker-copy :global(.model-badge) { max-width: 130px; margin-top: 2px; overflow: hidden; }
  .avatar { display: grid; flex: 0 0 auto; width: 25px; height: 25px; place-items: center; border-radius: 8px; background: hsl(var(--hue) 32% 88%); color: hsl(var(--hue) 30% 32%); font-size: 8px; font-weight: 750; }
  .worker-chip strong { font-size: 10px; }
  .status-dot { width: 6px; height: 6px; margin-left: auto; border-radius: 50%; background: var(--line-strong); }
  .status-dot[data-status="working"] { background: #73946a; }
  .status-dot[data-status="in_review"] { background: #b0884f; }
  .status-dot[data-status="fixing"] { background: #7d89aa; }
  .more-workers { align-self: center; color: var(--muted); font-size: 10px; }
  .card-action { display: inline-flex; align-items: center; gap: 6px; margin-top: 14px; padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: 10px; font-weight: 650; }
  @media (max-width: 560px) { .spawn-card { margin-left: 0; } .worker-chip { min-width: 100%; } }
</style>
