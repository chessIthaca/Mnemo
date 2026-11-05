<script lang="ts">
  import { formatElapsed } from "../lib/elapsed";

  /**
   * A clock from `startedAtMs`, live until `endedAtMs` freezes it. `prefix` is
   * what sits between the time and any label beside it (callers that draw no
   * label pass ""); `title` is the hover text.
   */
  let {
    startedAtMs,
    endedAtMs,
    prefix = " · ",
    title = "Elapsed time",
  }: { startedAtMs: number; endedAtMs?: number; prefix?: string; title?: string } = $props();
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

<span class="elapsed-time" aria-label={`Elapsed time ${elapsed}`} title={title}>{prefix}{elapsed}</span>

<style>
  .elapsed-time { white-space: nowrap; font-variant-numeric: tabular-nums; }
</style>
