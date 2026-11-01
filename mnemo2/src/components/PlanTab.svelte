<script lang="ts">
  import { Check, ChevronRight, Circle } from "@lucide/svelte";
  import { closedPlans, openPlans, planProgress, type BoardPlan } from "../lib/boardState";
  import { actions, app } from "../lib/store";
  import { workerStatusLabel, type WorkerStatus } from "../lib/types";
  import Collapsible from "./Collapsible.svelte";
  import Markdown from "./Markdown.svelte";

  const open = $derived(openPlans($app.board));
  const closed = $derived(closedPlans($app.board));

  const statusLabel = (status: string) => workerStatusLabel[status as WorkerStatus] ?? status;
</script>

{#snippet planCard(plan: BoardPlan)}
  {@const progress = planProgress(plan)}
  <article class="plan" data-status={plan.status} aria-label={plan.title}>
    <header class="plan-header">
      <p>
        <span class="kind">{plan.kind}</span>
        {#if plan.status !== "open"}<span class="state">{plan.status}</span>{/if}
        <span class="count">{progress.done}/{progress.total}</span>
      </p>
      <h2>{plan.title}</h2>
      {#if plan.goal}<Collapsible maxHeight={60} label={`${plan.title} goal`}><Markdown source={plan.goal} onOpenLink={actions.openBrowser} /></Collapsible>{/if}
      {#if plan.report_path}<code class="report">{plan.report_path}</code>{/if}
    </header>
    <ol class="steps">
      {#each plan.steps as step (step.number)}
        <li class="step" data-done={step.done}>
          <span class="marker">{#if step.done}<Check size={11} />{:else}<Circle size={11} />{/if}</span>
          <div class="step-copy">
            <Collapsible maxHeight={34} label={`step ${step.number}`}><span class="step-title">{step.header ?? step.text}</span></Collapsible>
            {#if step.workers.length > 0}
              <span class="chips">
                {#each step.workers as worker (worker.id)}
                  <button
                    class="chip"
                    data-status={worker.status}
                    aria-label={`Open ${worker.name} worker thread`}
                    onclick={() => actions.selectWorker(worker.id)}
                  >{worker.name} <small>{statusLabel(worker.status)}</small></button>
                {/each}
              </span>
            {/if}
          </div>
        </li>
      {/each}
    </ol>
  </article>
{/snippet}

<section class="plan-tab" aria-label="Plan">
  {#each open as plan (plan.id)}{@render planCard(plan)}{/each}

  {#if closed.length > 0}
    <details class="closed">
      <summary><ChevronRight size={11} /> Closed <span>{closed.length}</span></summary>
      {#each closed as plan (plan.id)}{@render planCard(plan)}{/each}
    </details>
  {/if}

  {#if open.length === 0 && closed.length === 0}
    <div class="plan-empty"><h2>No plans</h2></div>
  {/if}
</section>

<style>
  .plan-tab { padding: 14px 16px; }
  .plan { margin-bottom: 20px; }
  .plan[data-status="closed"], .plan[data-status="abandoned"] { opacity: 0.8; }
  .plan-header p { display: flex; align-items: center; gap: 8px; margin: 0 0 4px; color: var(--muted); font-size: var(--text-xs); letter-spacing: 0.08em; }
  .kind { padding: 2px 6px; border-radius: 6px; background: var(--surface-soft); text-transform: uppercase; }
  .state { text-transform: uppercase; }
  .count { margin-left: auto; font-variant-numeric: tabular-nums; }
  .plan-header h2 { margin: 0 0 4px; font-family: var(--font-prose); font-size: var(--text-lg); font-weight: 550; }
  .plan-header :global(.markdown) { color: var(--muted); font-size: var(--text-sm); line-height: 1.45; }
  .plan-header :global(.markdown h1), .plan-header :global(.markdown h2), .plan-header :global(.markdown h3) { font-size: var(--text-md); }
  .plan-header :global(.markdown h4), .plan-header :global(.markdown h5), .plan-header :global(.markdown h6) { font-size: var(--text-sm); }
  .report { display: block; margin-top: 6px; color: var(--muted); font-size: var(--text-sm); }
  .plan-empty { padding: 40px 4px; color: var(--muted); }
  .plan-empty h2 { margin: 0; color: var(--ink); font-family: var(--font-prose); font-size: var(--text-xl); font-weight: 550; }
  .steps { margin: 10px 0 0; padding: 0; list-style: none; }
  .step { display: flex; align-items: flex-start; gap: 8px; padding: 6px 0; border-top: 1px solid var(--line); }
  .marker { display: grid; flex: 0 0 auto; width: 16px; height: 16px; place-items: center; border: 1px solid var(--line); border-radius: 6px; color: var(--muted); }
  .step[data-done="true"] .marker { border-color: #d6e0d0; background: var(--accent-soft); color: var(--accent-strong); }
  .step-copy { flex: 1; min-width: 0; }
  .step-title { font-size: var(--text-sm); line-height: 1.5; }
  .step[data-done="true"] .step-title { color: var(--muted); }
  .chips { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 6px; }
  .chip { display: inline-flex; align-items: center; gap: 4px; padding: 2px 7px; border: 1px solid var(--line); border-radius: 999px; background: transparent; color: var(--ink-soft); font-size: var(--text-xs); cursor: pointer; }
  .chip:hover { background: var(--surface-soft); }
  .chip small { color: var(--muted); font-size: var(--text-2xs); }
  .chip[data-status="merged"] small { color: var(--accent-strong); }
  .chip[data-status="failed"] small, .chip[data-status="conflict"] small { color: var(--danger); }
  .closed > summary { display: flex; align-items: center; gap: 5px; margin-bottom: 14px; color: var(--muted); font-size: var(--text-sm); cursor: pointer; list-style: none; }
  .closed > summary::-webkit-details-marker { display: none; }
  .closed[open] > summary :global(svg) { transform: rotate(90deg); }
</style>
