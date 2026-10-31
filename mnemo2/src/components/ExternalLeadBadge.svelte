<script lang="ts">
  import { app } from "../lib/store";

  const attached = $derived($app.externalLead.attached);
</script>

{#if attached}
  <span
    class="external-lead-badge"
    title="Claude Code is leading this project through the mnemo plugin. Mnemo2 sends worker notices to it instead of the built-in lead."
  ><span class="dot" aria-hidden="true"></span><span>Lead: Claude Code (external)</span></span>
{/if}

<style>
  .external-lead-badge { display: inline-flex; align-items: center; gap: 5px; padding: 4px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--accent-soft); color: var(--accent-strong); font-size: 9px; font-weight: 600; line-height: 1.2; white-space: nowrap; }
  .dot { width: 4px; height: 4px; flex: 0 0 auto; border-radius: 50%; background: var(--accent); }
</style>
