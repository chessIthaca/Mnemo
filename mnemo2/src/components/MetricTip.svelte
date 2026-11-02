<script lang="ts">
  // Copyright (c) 2026 Carsten Hess
  // SPDX-License-Identifier: MIT
  // See LICENSE in the repository root.

  import { Info } from "@lucide/svelte";
  import { TRACE_GLOSSARY, type MetricTerm } from "../lib/trace";

  /**
   * One glossary entry as a hover/focus tip: the friendly label the text
   * already shows, the plain-language explanation a keypress away (§7 —
   * every metric explained on hover, keyboard included).
   */
  let {
    /** Which glossary entry to explain. */
    term,
    /** The label the surrounding text shows; defaults to the glossary's term. */
    label,
  }: { term: MetricTerm; label?: string } = $props();

  const gloss = $derived(TRACE_GLOSSARY[term]);
  const shownLabel = $derived(label ?? gloss.term);
  const tipId = $derived(`metric-tip-${term}`);
  let open = $state(false);
</script>

<span class="metric-tip">
  <button
    type="button"
    class="tip-button"
    aria-label={`What does “${shownLabel}” mean?`}
    aria-expanded={open}
    aria-describedby={open ? tipId : undefined}
    onmouseenter={() => (open = true)}
    onmouseleave={() => (open = false)}
    onfocus={() => (open = true)}
    onblur={() => (open = false)}
  >
    <Info size={12} aria-hidden="true" />
  </button>
  <span id={tipId} class="tip-body" class:open role="tooltip">
    <strong>{gloss.term}</strong>
    {gloss.body}
  </span>
</span>

<style>
  .metric-tip { position: relative; display: inline-flex; align-items: center; }
  .tip-button { display: inline-flex; padding: 0; border: 0; border-radius: 5px; background: transparent; color: var(--muted); cursor: help; }
  .tip-button:hover, .tip-button:focus-visible { color: var(--accent-strong); }
  .tip-button:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .tip-body { position: absolute; top: calc(100% + 6px); left: 0; z-index: 40; width: 240px; padding: 9px 11px; border: 1px solid var(--line-strong); border-radius: 10px; background: var(--surface); box-shadow: var(--shadow); color: var(--ink-soft); font-size: var(--text-meta); line-height: 1.5; opacity: 0; visibility: hidden; pointer-events: none; transition: opacity 120ms ease; }
  .tip-body.open { opacity: 1; visibility: visible; }
  .tip-body strong { display: block; margin-bottom: 4px; color: var(--ink); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.04em; text-transform: uppercase; }
</style>