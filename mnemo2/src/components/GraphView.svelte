<script lang="ts">
  import { Network } from "@lucide/svelte";
</script>

<section class="insight-view" aria-label="Code graph">
  <header class="insight-header">
    <h1>Code graph</h1>
    <p>Search symbols and explore callers, callees and change impact — coming next.</p>
  </header>
  <div class="insight-placeholder">
    <Network size={22} />
    <p>The graph explorer lands in a later step of this plan.</p>
  </div>
</section>

<style>
  .insight-view { display: flex; flex: 1; min-width: 0; min-height: 0; flex-direction: column; background: var(--canvas); }
  .insight-header { padding: 18px 24px 12px; }
  .insight-header h1 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 500; letter-spacing: -0.03em; }
  .insight-header p { margin: 6px 0 0; color: var(--muted); font-size: var(--text-body); }
  .insight-placeholder { display: grid; flex: 1; place-items: center; align-content: center; gap: 10px; padding: 40px 24px; border-top: 1px solid var(--line); color: var(--muted); text-align: center; }
  .insight-placeholder p { margin: 0; font-size: var(--text-body); }
</style>
