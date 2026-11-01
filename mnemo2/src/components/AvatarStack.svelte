<script lang="ts">
  import { briefSummary } from "../lib/brief";
  import type { Worker } from "../lib/types";

  let { workers, max = 4 }: { workers: Worker[]; max?: number } = $props();
</script>

<span class="avatar-stack" aria-label={`${workers.length} workers`}>
  {#each workers.slice(0, max) as worker (worker.id)}
    <span class="avatar" style="--hue: {worker.hue}" title={`${worker.name} · ${briefSummary(worker.task)}`}>{worker.initials}</span>
  {/each}
  {#if workers.length > max}<span class="overflow">+{workers.length - max}</span>{/if}
</span>

<style>
  .avatar-stack { display: inline-flex; align-items: center; padding-left: 7px; }
  .avatar, .overflow { display: grid; width: 26px; height: 26px; margin-left: -7px; place-items: center; border: 2px solid var(--canvas); border-radius: 50%; font-size: var(--text-meta); font-weight: 750; }
  .avatar { background: hsl(var(--hue) 32% 85%); color: hsl(var(--hue) 30% 32%); }
  .overflow { background: var(--surface-soft); color: var(--muted); }
</style>
