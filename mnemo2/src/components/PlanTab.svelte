<script lang="ts">
  import { Check, Circle, LoaderCircle } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import Markdown from "./Markdown.svelte";
</script>

<section class="plan-tab" aria-label="Plan">
  {#if $app.plan}
    <header class="plan-header">
      <p>LEAD PLAN · {$app.plan.id}</p>
      <h2>{$app.plan.title}</h2>
      <Markdown source={$app.plan.goal} onOpenLink={actions.openBrowser} />
    </header>

    <div class="plan-list">
      {#each $app.planItems as item (item.id)}
        <article class="plan-item" data-status={item.status}>
          <span class="plan-marker">
            {#if item.status === "done"}<Check size={11} />
            {:else if item.status === "active"}<LoaderCircle size={11} />
            {:else}<Circle size={11} />{/if}
          </span>
          <div><h3>{item.title}</h3><Markdown source={item.detail} onOpenLink={actions.openBrowser} /><span>OWNER · LEAD</span></div>
          <span class="plan-status">{item.status}</span>
        </article>
      {:else}
        <p class="plan-note">This plan has no steps.</p>
      {/each}
    </div>
  {:else}
    <div class="plan-empty">
      <h2>No active plan</h2>
      <p>The lead has not created a plan in this session.</p>
    </div>
  {/if}
</section>

<style>
  .plan-tab { padding: 18px; }
  .plan-header p { margin: 0 0 4px; color: var(--muted); font-size: 9px; letter-spacing: 0.11em; }
  .plan-header h2 { margin: 0 0 5px; font-family: var(--font-prose); font-size: 26px; font-weight: 550; }
  .plan-header :global(.markdown) { color: var(--muted); font-size: 13px; line-height: 1.5; }
  .plan-empty { padding: 40px 4px; color: var(--muted); }
  .plan-empty h2 { margin: 0 0 8px; color: var(--ink); font-family: var(--font-prose); font-size: 26px; font-weight: 550; }
  .plan-empty p { font-size: 12px; line-height: 1.6; }
  .plan-list { margin-top: 18px; }
  .plan-item { display: flex; align-items: flex-start; gap: 10px; padding: 12px 0; border-top: 1px solid var(--line); }
  .plan-marker { display: grid; flex: 0 0 auto; width: 22px; height: 22px; place-items: center; border: 1px solid var(--line); border-radius: 7px; color: var(--muted); }
  .plan-item[data-status="done"] .plan-marker { border-color: #d6e0d0; background: var(--accent-soft); color: var(--accent-strong); }
  .plan-item[data-status="active"] .plan-marker { border-color: #e3d6c1; background: var(--warning-soft); color: var(--warning); }
  .plan-item div { flex: 1; min-width: 0; }
  .plan-item h3 { margin: 1px 0 3px; font-size: 10px; font-weight: 650; }
  .plan-item :global(.markdown) { margin: 0 0 4px; color: var(--muted); font-size: 13px; line-height: 1.5; }
  .plan-item div > span { color: #a0a398; font-size: 8px; letter-spacing: 0.08em; }
  .plan-status { color: var(--muted); font-size: 9px; text-transform: uppercase; }
  .plan-item[data-status="done"] .plan-status { color: var(--accent-strong); }
  .plan-note { margin: 16px 2px 4px; color: var(--muted); font-size: 8px; line-height: 1.5; }
</style>
