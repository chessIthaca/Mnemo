<script lang="ts">
  import { formatElapsed } from "../lib/elapsed";

  let { startedAtMs, endedAtMs }: { startedAtMs: number; endedAtMs?: number } = $props();
  let nowMs = $state(Date.now());

  $effect(() => {
    if (endedAtMs !== undefined) {
      nowMs = endedAtMs;
      return;
    }
    nowMs = Date.now();
    const interval = window.setInterval(() => {
      nowMs = Date.now();
    }, 1000);
    return () => window.clearInterval(interval);
  });

  const elapsed = $derived(formatElapsed(startedAtMs, endedAtMs ?? nowMs));
</script>

<span class="elapsed-time" aria-label={`Elapsed time ${elapsed}`} title="Elapsed time"> · {elapsed}</span>

<style>
  .elapsed-time { white-space: nowrap; font-variant-numeric: tabular-nums; }
</style>
