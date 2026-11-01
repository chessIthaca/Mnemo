<script lang="ts">
  import type { Snippet } from "svelte";
  import { ChevronDown } from "@lucide/svelte";

  let {
    maxHeight = 96,
    label = "text",
    children,
  }: {
    /** Height in px the content is clamped to while collapsed. */
    maxHeight?: number;
    /** What is clamped, for the toggle's accessible name ("plan goal"). */
    label?: string;
    children: Snippet;
  } = $props();

  let expanded = $state(false);
  let content = $state<HTMLDivElement | null>(null);
  let overflowing = $state(false);

  // Only content taller than the clamp gets a toggle; re-measured on resize
  // and whenever the content itself grows (streaming, markdown re-render).
  $effect(() => {
    const element = content;
    if (!element) return;
    const measure = () => {
      overflowing = element.scrollHeight > maxHeight + 8;
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    for (const child of Array.from(element.children)) observer.observe(child);
    return () => observer.disconnect();
  });
</script>

<div class="collapsible" class:clamped={overflowing && !expanded}>
  <div
    class="collapsible-content"
    bind:this={content}
    style:max-height={overflowing && !expanded ? `${maxHeight}px` : null}
  >
    {@render children()}
  </div>
  {#if overflowing}
    <button
      class="collapsible-toggle"
      class:expanded
      aria-expanded={expanded}
      aria-label={`${expanded ? "Collapse" : "Expand"} ${label}`}
      onclick={() => (expanded = !expanded)}
    ><ChevronDown size={11} /> {expanded ? "Show less" : "Show more"}</button>
  {/if}
</div>

<style>
  .collapsible-content { overflow: hidden; }
  .clamped .collapsible-content {
    -webkit-mask-image: linear-gradient(to bottom, #000 60%, transparent);
    mask-image: linear-gradient(to bottom, #000 60%, transparent);
  }
  .collapsible-toggle { display: inline-flex; align-items: center; gap: 3px; margin-top: 3px; padding: 0; border: 0; background: transparent; color: var(--accent-strong); font-size: var(--text-xs); font-weight: 600; cursor: pointer; }
  .collapsible-toggle :global(svg) { transition: transform 120ms ease; }
  .collapsible-toggle.expanded :global(svg) { transform: rotate(180deg); }
</style>
