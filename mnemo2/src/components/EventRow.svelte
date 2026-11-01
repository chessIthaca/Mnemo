<script lang="ts">
  import { ChevronRight } from "@lucide/svelte";
  import type { Snippet } from "svelte";

  /**
   * A team event in the lead conversation (merge, review, worker update): an
   * icon and a headline, with the details one click away.
   */
  let {
    icon,
    heading,
    label,
    tone = "neutral",
    children,
  }: {
    icon: Snippet;
    heading: Snippet;
    /** The accessible name of the row (the headline as plain text). */
    label: string;
    tone?: "neutral" | "merge" | "review";
    children: Snippet;
  } = $props();
  let open = $state(false);
</script>

<details class="event-row" data-tone={tone} bind:open>
  <summary aria-label={label}>
    <span class="event-icon">{@render icon()}</span>
    <span class="event-heading">{@render heading()}</span>
    <ChevronRight class="event-chevron" size={12} aria-hidden="true" />
  </summary>
  {#if open}
    <div class="event-body">{@render children()}</div>
  {/if}
</details>

<style>
  .event-row { min-width: 0; margin: 10px 0 10px 30px; }
  summary { display: flex; min-width: 0; align-items: center; gap: 8px; border-radius: 4px; cursor: pointer; list-style: none; }
  summary::-webkit-details-marker { display: none; }
  summary:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  .event-icon { display: grid; width: 22px; height: 22px; flex: 0 0 auto; place-items: center; border-radius: 7px; background: var(--surface-soft); color: var(--ink-soft); }
  [data-tone="merge"] .event-icon { background: #e2ebdc; color: var(--accent-strong); }
  [data-tone="review"] .event-icon { background: #ece8de; color: #6f6046; }
  .event-heading { min-width: 0; overflow: hidden; color: var(--ink); font-size: var(--text-md); font-weight: 600; line-height: 1.5; text-overflow: ellipsis; white-space: nowrap; }
  summary:hover .event-heading { color: var(--accent-strong); }
  summary :global(.event-chevron) { flex: 0 0 auto; margin-left: auto; color: var(--muted); transition: transform 120ms ease; }
  .event-row[open] summary :global(.event-chevron) { transform: rotate(90deg); }
  .event-body { margin: 6px 0 4px 30px; color: var(--ink-soft); font-size: var(--text-sm); }
  @media (max-width: 560px) { .event-row { margin-left: 0; } }
</style>
