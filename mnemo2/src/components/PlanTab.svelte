<script lang="ts">
  import { onMount, tick } from "svelte";
  import { ArrowUpRight, Check, ChevronDown, ChevronRight, Circle } from "@lucide/svelte";
  import { createBoardTransport } from "../lib/boardEngine";
  import { closedPlans, openPlans, planProgress, type BoardPlan } from "../lib/boardState";
  import { planDetailMarkdown } from "../lib/planDocument";
  import { actions, app } from "../lib/store";
  import { workerStatusLabel, type WorkerStatus } from "../lib/types";
  import Collapsible from "./Collapsible.svelte";
  import Markdown from "./Markdown.svelte";

  /**
   * Which plans the tab lists: `"all"` for the Plans route (open and closed),
   * `"open"` for the Workbench tab, which shows only work in progress and
   * links out to the full list.
   */
  interface Props {
    scope?: "all" | "open";
  }
  let { scope = "all" }: Props = $props();

  const open = $derived(openPlans($app.board));
  const closed = $derived(scope === "all" ? closedPlans($app.board) : []);

  const statusLabel = (status: string) => workerStatusLabel[status as WorkerStatus] ?? status;

  /** One plan's expanded details: the raw document and its load state. */
  interface PlanDetails {
    markdown: string;
    loading: boolean;
    error: string;
  }

  const transport = createBoardTransport();
  let expanded = $state<Record<string, boolean>>({});
  let details = $state<Record<string, PlanDetails>>({});
  /** A handed-over plan id the board does not list, for the not-found note. */
  let missingPlan = $state("");
  /** The tab's root, used to bring a handed-over plan's card into view. */
  let tabSection = $state<HTMLElement | null>(null);

  const isExpanded = (plan: BoardPlan) => expanded[plan.id] === true;

  /** Flatten a rejected invoke (string or Error) to display text. */
  const errorText = (error: unknown) =>
    typeof error === "string" ? error : error instanceof Error ? error.message : "Could not read the plan";

  /** The details Markdown: the card already shows title, goal and step headers. */
  const detailSource = (plan: BoardPlan) =>
    planDetailMarkdown(
      details[plan.id]?.markdown ?? "",
      plan.steps.some((step) => (step.header ?? "").trim() !== ""),
    );

  async function loadDetails(plan: BoardPlan) {
    const active = transport;
    if (!active) {
      details[plan.id] = { markdown: "", loading: false, error: "Plan details need the desktop app." };
      return;
    }
    details[plan.id] = { markdown: details[plan.id]?.markdown ?? "", loading: true, error: "" };
    try {
      const markdown = await active.document(plan.id);
      details[plan.id] = { markdown, loading: false, error: "" };
    } catch (error) {
      details[plan.id] = { markdown: "", loading: false, error: errorText(error) };
    }
  }

  function toggleDetails(plan: BoardPlan) {
    const opening = !isExpanded(plan);
    expanded[plan.id] = opening;
    if (opening && !details[plan.id]?.markdown) void loadDetails(plan);
  }

  /**
   * A plan id the Memory tab handed over: expand it, read its document and
   * scroll it into view once the board lists it. An id the board never lists
   * shows a short note instead, so the click is never a silent no-op.
   */
  $effect(() => {
    const id = $app.planDocumentId;
    if (id === null || scope !== "all") return;
    const plan = $app.board.find((entry) => entry.id === id);
    if (!plan) {
      // The board syncs when a project opens: an empty one is not proof.
      if ($app.board.length === 0) return;
      actions.clearPlanDocument();
      missingPlan = id;
      return;
    }
    actions.clearPlanDocument();
    missingPlan = "";
    expanded[plan.id] = true;
    if (!details[plan.id]?.markdown) void loadDetails(plan);
    void scrollToPlan(plan.id);
  });

  /** Bring an expanded plan's card into view, after its details render. */
  async function scrollToPlan(planId: string) {
    await tick();
    const card = tabSection?.querySelector<HTMLElement>(`[data-plan-id="${planId}"]`);
    card?.scrollIntoView?.({ block: "center", behavior: "smooth" });
  }

  // An open card re-reads its document on the board change, so the details
  // follow edits the lead makes while the tab stays open.
  onMount(() => {
    const active = transport;
    if (!active) return;
    let live = true;
    let unlisten: (() => void) | null = null;
    void active.onChanged(() => {
      if (!live) return;
      for (const plan of $app.board) if (expanded[plan.id]) void loadDetails(plan);
    }).then((stop) => {
      if (live) unlisten = stop;
      else stop();
    });
    return () => {
      live = false;
      unlisten?.();
    };
  });
</script>

{#snippet planCard(plan: BoardPlan)}
  {@const progress = planProgress(plan)}
  <article class="plan" data-plan-id={plan.id} data-status={plan.status} aria-label={plan.title}>
    <header class="plan-header">
      <p>
        <span class="kind">{plan.kind}</span>
        {#if plan.status !== "open"}<span class="state">{plan.status}</span>{/if}
        <span class="count">{progress.done}/{progress.total}</span>
        <button
          class="details-toggle"
          aria-expanded={isExpanded(plan)}
          aria-controls={`plan-details-${plan.id}`}
          onclick={() => toggleDetails(plan)}
        ><ChevronDown size={11} /> Details</button>
      </p>
      <h2>{plan.title}</h2>
      {#if plan.goal}<Collapsible maxHeight={60} label={`${plan.title} goal`}><Markdown source={plan.goal} onOpenLink={actions.openBrowser} onOpenPath={actions.openRepoPath} /></Collapsible>{/if}
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
    {#if isExpanded(plan)}
      <section class="plan-details" id={`plan-details-${plan.id}`} aria-label={`${plan.title} details`}>
        {#if details[plan.id]?.loading}
          <p class="details-note">Loading plan…</p>
        {:else if details[plan.id]?.error}
          <p class="details-note details-error">{details[plan.id].error}</p>
        {:else}
          <Markdown source={detailSource(plan)} onOpenLink={actions.openBrowser} onOpenPath={actions.openRepoPath} />
        {/if}
      </section>
    {/if}
  </article>
{/snippet}

<section class="plan-tab" aria-label="Plan" bind:this={tabSection}>
  {#if scope === "open"}
    <p class="scope-header">
      <button class="all-plans" type="button" onclick={actions.openPlans}>All plans <ArrowUpRight size={11} /></button>
    </p>
  {/if}

  {#if missingPlan !== ""}
    <p class="plan-missing" role="status">Plan “{missingPlan}” is not on this project's board.</p>
  {/if}

  {#each open as plan (plan.id)}{@render planCard(plan)}{/each}

  {#if closed.length > 0}
    <details class="closed">
      <summary><ChevronRight size={11} /> Closed <span>{closed.length}</span></summary>
      {#each closed as plan (plan.id)}{@render planCard(plan)}{/each}
    </details>
  {/if}

  {#if open.length === 0 && closed.length === 0}
    <div class="plan-empty"><h2>{scope === "open" ? "No open plans" : "No plans"}</h2></div>
  {/if}
</section>

<style>
  .plan-tab { padding: 14px 16px; }
  .scope-header { display: flex; justify-content: flex-end; margin: 0 0 8px; }
  .all-plans { display: inline-flex; align-items: center; gap: 4px; padding: 2px 0; border: 0; background: transparent; color: var(--accent-strong); font-size: var(--text-body); font-weight: 650; cursor: pointer; }
  .all-plans:hover { text-decoration: underline; }
  .all-plans:focus-visible { outline: 2px solid var(--accent-strong); outline-offset: 2px; }
  .plan { margin-bottom: 20px; }
  .plan[data-status="closed"], .plan[data-status="abandoned"] { opacity: 0.8; }
  .plan-header p { display: flex; align-items: center; gap: 8px; margin: 0 0 4px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.08em; }
  .kind { padding: 2px 6px; border-radius: 6px; background: var(--surface-soft); text-transform: uppercase; }
  .state { text-transform: uppercase; }
  .count { margin-left: auto; font-variant-numeric: tabular-nums; }
  .details-toggle { display: inline-flex; align-items: center; gap: 3px; padding: 2px 6px; border: 1px solid transparent; border-radius: 6px; background: transparent; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.06em; text-transform: uppercase; cursor: pointer; }
  .details-toggle:hover { border-color: var(--line); background: var(--surface-soft); color: var(--ink-soft); }
  .details-toggle:focus-visible { outline: 2px solid var(--accent-strong); outline-offset: 1px; }
  .details-toggle :global(svg) { transition: transform 120ms ease; }
  .details-toggle[aria-expanded="true"] :global(svg) { transform: rotate(180deg); }
  .plan-details { margin-top: 10px; padding: 12px 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
  .plan-details :global(.markdown) { font-size: var(--text-body); line-height: 1.55; }
  .plan-details :global(.markdown h2) { margin: 16px 0 6px; font-size: var(--text-title); font-weight: 620; }
  .plan-details :global(.markdown h2:first-child) { margin-top: 0; }
  .plan-details :global(.markdown h3) { margin: 12px 0 4px; font-size: var(--text-body); font-weight: 650; }
  .details-note { margin: 0; color: var(--muted); font-size: var(--text-body); }
  .details-error { color: var(--danger); }
  .plan-header h2 { margin: 0 0 4px; font-family: var(--font-prose); font-size: var(--text-title); font-weight: 550; }
  .plan-header :global(.markdown) { color: var(--muted); font-size: var(--text-body); line-height: 1.45; }
  .plan-header :global(.markdown h1), .plan-header :global(.markdown h2), .plan-header :global(.markdown h3) { font-size: var(--text-body); }
  .plan-header :global(.markdown h4), .plan-header :global(.markdown h5), .plan-header :global(.markdown h6) { font-size: var(--text-body); }
  .report { display: block; margin-top: 6px; color: var(--muted); font-size: var(--text-body); }
  .plan-empty { padding: 40px 4px; color: var(--muted); }
  .plan-missing { margin: 0 0 14px; padding: 9px 11px; border: 1px solid var(--line); border-radius: 9px; background: var(--warning-soft); color: var(--warning); font-size: var(--text-body); }
  .plan-empty h2 { margin: 0; color: var(--ink); font-family: var(--font-prose); font-size: var(--text-page); font-weight: 550; }
  .steps { margin: 10px 0 0; padding: 0; list-style: none; }
  .step { display: flex; align-items: flex-start; gap: 8px; padding: 6px 0; border-top: 1px solid var(--line); }
  .marker { display: grid; flex: 0 0 auto; width: 16px; height: 16px; place-items: center; border: 1px solid var(--line); border-radius: 6px; color: var(--muted); }
  .step[data-done="true"] .marker { border-color: var(--accent-line); background: var(--accent-soft); color: var(--accent-strong); }
  .step-copy { flex: 1; min-width: 0; }
  .step-title { font-size: var(--text-body); line-height: 1.5; }
  .step[data-done="true"] .step-title { color: var(--muted); }
  .chips { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 6px; }
  .chip { display: inline-flex; align-items: center; gap: 4px; padding: 2px 7px; border: 1px solid var(--line); border-radius: 999px; background: transparent; color: var(--ink-soft); font-size: var(--text-meta); cursor: pointer; }
  .chip:hover { background: var(--surface-soft); }
  .chip small { color: var(--muted); font-size: var(--text-meta); }
  .chip[data-status="merged"] small { color: var(--accent-strong); }
  .chip[data-status="failed"] small, .chip[data-status="conflict"] small { color: var(--danger); }
  .closed > summary { display: flex; align-items: center; gap: 5px; margin-bottom: 14px; color: var(--muted); font-size: var(--text-body); cursor: pointer; list-style: none; }
  .closed > summary::-webkit-details-marker { display: none; }
  .closed[open] > summary :global(svg) { transform: rotate(90deg); }
</style>
