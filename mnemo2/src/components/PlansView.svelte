<script lang="ts">
  import BacklogSection from "./BacklogSection.svelte";
  import PlanTab from "./PlanTab.svelte";
</script>

<section class="plans-view" aria-label="Plans">
  <header class="plans-header">
    <h1>Plans</h1>
    <p>Ideas waiting to become plans, then every board plan — open and closed — with the whole document one click away.</p>
  </header>
  <div class="plans-scroll">
    <BacklogSection />
    <PlanTab />
  </div>
</section>

<style>
  .plans-view { display: flex; flex: 1; min-width: 0; min-height: 0; flex-direction: column; background: var(--canvas); }
  .plans-header { padding: 18px 24px 12px; }
  .plans-header h1 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 500; letter-spacing: -0.03em; }
  .plans-header p { margin: 6px 0 0; color: var(--muted); font-size: var(--text-body); }
  .plans-scroll { flex: 1; min-height: 0; overflow-y: auto; border-top: 1px solid var(--line); }
  .plans-scroll :global(.backlog) { max-width: 880px; padding: 18px 24px 0; }
  .plans-scroll :global(.plan-tab) { max-width: 880px; padding: 18px 24px 32px; }
</style>
