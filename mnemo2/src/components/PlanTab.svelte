<script lang="ts">
  import { Check, Circle, LoaderCircle } from "@lucide/svelte";
  import { app } from "../lib/store";
</script>

<section class="plan-tab" aria-label="Plan">
  <header class="plan-header">
    <p>ONE PLAN OF RECORD</p>
    <h2>The lead’s plan</h2>
    <span>Workers receive a brief, not a second planning loop. The plan stays here.</span>
  </header>

  <div class="plan-list">
    {#each $app.planItems as item (item.id)}
      <article class="plan-item" data-status={item.status}>
        <span class="plan-marker">
          {#if item.status === "done"}<Check size={11} />
          {:else if item.status === "active"}<LoaderCircle size={11} />
          {:else}<Circle size={11} />{/if}
        </span>
        <div><h3>{item.title}</h3><p>{item.detail}</p><span>OWNER · {item.owner.toUpperCase()}</span></div>
        <span class="plan-status">{item.status}</span>
      </article>
    {/each}
  </div>
  <p class="plan-note">Mock plan frame. Phase 2 connects it to the lead’s real plan and worker briefs.</p>
</section>

<style>
  .plan-tab { padding: 18px; }
  .plan-header p { margin: 0 0 4px; color: var(--muted); font-size: 7px; letter-spacing: 0.11em; }
  .plan-header h2 { margin: 0 0 5px; font-family: var(--font-prose); font-size: 26px; font-weight: 550; }
  .plan-header > span { color: var(--muted); font-size: 9px; }
  .plan-list { margin-top: 18px; }
  .plan-item { display: flex; align-items: flex-start; gap: 10px; padding: 12px 0; border-top: 1px solid var(--line); }
  .plan-marker { display: grid; flex: 0 0 auto; width: 22px; height: 22px; place-items: center; border: 1px solid var(--line); border-radius: 7px; color: var(--muted); }
  .plan-item[data-status="done"] .plan-marker { border-color: #d6e0d0; background: var(--accent-soft); color: var(--accent-strong); }
  .plan-item[data-status="active"] .plan-marker { border-color: #e3d6c1; background: var(--warning-soft); color: var(--warning); }
  .plan-item div { flex: 1; min-width: 0; }
  .plan-item h3 { margin: 1px 0 3px; font-size: 10px; font-weight: 650; }
  .plan-item p { margin: 0 0 4px; color: var(--muted); font-family: var(--font-prose); font-size: 12px; line-height: 1.45; }
  .plan-item div > span { color: #a0a398; font-size: 6px; letter-spacing: 0.08em; }
  .plan-status { color: var(--muted); font-size: 7px; text-transform: uppercase; }
  .plan-item[data-status="done"] .plan-status { color: var(--accent-strong); }
  .plan-note { margin: 16px 2px 4px; color: var(--muted); font-size: 8px; line-height: 1.5; }
</style>
