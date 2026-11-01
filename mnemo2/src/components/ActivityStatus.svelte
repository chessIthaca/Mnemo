<script lang="ts">
  import ElapsedTime from "./ElapsedTime.svelte";

  /**
   * The live status row the lead chat and the worker threads share: a pulsing
   * dot, the phase label, and the elapsed timer of the span it describes.
   * `prefers-reduced-motion` stops the pulse and leaves the dot solid.
   */
  let {
    label,
    startedAtMs = null,
    pulse = true,
    size = "body",
  }: {
    /** The status label, e.g. "Thinking…". */
    label: string;
    /** When the timed span began (ms epoch); null omits the timer. */
    startedAtMs?: number | null;
    /** False keeps the dot solid — phases that wait on the human do not pulse. */
    pulse?: boolean;
    /** `meta` for worker rows, `body` for threads — the only two text tokens used. */
    size?: "body" | "meta";
  } = $props();
</script>

<div class="thinking-status" class:meta={size === "meta"} class:still={!pulse} role="status" aria-live="off">
  <span class="thinking-dot" aria-hidden="true"></span>
  <span>{label}</span>
  {#if startedAtMs !== null}
    <ElapsedTime {startedAtMs} />
  {/if}
</div>

<style>
  .thinking-status { display: flex; align-items: center; gap: 6px; color: var(--muted); font-size: var(--text-body); }
  .thinking-status.meta { font-size: var(--text-meta); }
  .thinking-dot { flex: 0 0 auto; width: 5px; height: 5px; border-radius: 50%; background: var(--accent); animation: thinking-pulse 1.2s ease-in-out infinite; }
  .still .thinking-dot { animation: none; opacity: 1; }
  @keyframes thinking-pulse { 0%, 100% { opacity: 0.35; } 50% { opacity: 1; } }
  @media (prefers-reduced-motion: reduce) {
    .thinking-dot { animation: none; opacity: 1; }
  }
</style>