<script lang="ts">
  import { onMount } from "svelte";
  import { ArrowLeft, ArrowUpRight, X } from "@lucide/svelte";
  import { workerPlanLabel } from "../lib/boardState";
  import { actions, app } from "../lib/store";
  import { workerStatusText, type Worker, type WorkerStatus } from "../lib/types";
  import ModelBadge from "./ModelBadge.svelte";
  import WorkerThread from "./WorkerThread.svelte";

  /**
   * Streams a worker's output beside the lead chat. The header picks between
   * every active worker (plus the shown one, even once it finishes), shows its
   * model and status, and hands off to Mission Control; the body is the same
   * live thread Mission Control shows, with a reply box that steers the worker.
   */
  const PANEL_STATUSES: WorkerStatus[] = [
    "dispatching",
    "working",
    "fixing",
    "idle",
    "needs_you",
    "paused",
    "interrupted",
  ];
  /** A blocked worker the panel can still offer a way out for. */
  const BLOCKED_STATUSES: WorkerStatus[] = ["failed", "conflict", "stopped", "interrupted"];

  const worker = $derived($app.workers.find((item) => item.id === $app.workerPanelId) ?? null);
  const planLabel = $derived(worker ? workerPlanLabel($app.board, worker) : null);
  const blocked = $derived(worker !== null && BLOCKED_STATUSES.includes(worker.status));
  // Active and needs-you workers, always keeping the shown worker listed so a
  // finished worker never jumps the panel away on its own.
  const selectable = $derived.by(() => {
    const actives = $app.workers.filter(
      (item) => PANEL_STATUSES.includes(item.status) || item.needsYou,
    );
    if (!worker || actives.some((item) => item.id === worker.id)) return actives;
    return [worker, ...actives];
  });

  let resizing = $state(false);

  function switchWorker(event: Event) {
    const id = (event.currentTarget as HTMLSelectElement).value;
    if (id !== $app.workerPanelId) actions.openWorkerPanel(id);
  }

  function startResize(event: PointerEvent) {
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    resizing = true;
  }

  function handleMove(event: PointerEvent) {
    if (!resizing) return;
    actions.setWorkbenchWidth(window.innerWidth - event.clientX);
  }

  function stopResize() {
    resizing = false;
  }

  // A saved wide panel yields to the lead when the window cannot hold both.
  onMount(() => actions.reclampWorkbenchWidth());

  function resizeKeydown(event: KeyboardEvent) {
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      actions.setWorkbenchWidth($app.workbenchWidth + 24);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      actions.setWorkbenchWidth($app.workbenchWidth - 24);
    }
  }
</script>

<svelte:window onpointermove={handleMove} onpointerup={stopResize} onpointercancel={stopResize} onresize={() => actions.reclampWorkbenchWidth()} />

{#if worker}
  <aside class="worker-panel" style={`width: ${$app.workbenchWidth}px`} aria-label="Worker output">
    <button
      class="resizer"
      class:resizing
      type="button"
      aria-label="Resize worker panel"
      title="Drag or use the arrow keys to resize"
      onpointerdown={startResize}
      onkeydown={resizeKeydown}
    ></button>

    <header class="panel-header">
      <button class="back-button" aria-label="Close worker panel" onclick={actions.closeWorkerPanel}><ArrowLeft size={15} /></button>
      <div class="panel-heading">
        <select aria-label="Choose worker" value={$app.workerPanelId} onchange={switchWorker}>
          {#each selectable as choice (choice.id)}
            <option value={choice.id}>{choice.name}</option>
          {/each}
        </select>
        <span class="panel-status" data-status={worker.status}>{workerStatusText(worker)}</span>
      </div>
      <div class="panel-meta">
        <ModelBadge model={worker.model} />
        {#if worker.reasoningEffort}<span class="pill">effort: {worker.reasoningEffort}</span>{/if}
        {#if worker.tier}<span class="pill">tier: {worker.tier}</span>{/if}
        {#if planLabel}<span class="pill plan">{planLabel}</span>{/if}
      </div>
      <div class="panel-actions">
        <button class="handoff-button" type="button" aria-label="Open in Mission Control" onclick={() => actions.selectWorker(worker.id)}>
          <ArrowUpRight size={13} /> Mission Control
        </button>
        <button class="close-button" aria-label="Close worker panel" onclick={actions.closeWorkerPanel}><X size={15} /></button>
      </div>
    </header>

    {#if blocked}
      <div class="panel-banner" role="status" data-status={worker.status}>
        {#if worker.summary.trim()}<p class="banner-error">{worker.summary}</p>{/if}
        <div class="banner-actions">
          <button class="banner-button continue" type="button" onclick={() => actions.resumeWorker(worker.id)}>Continue</button>
          <button class="banner-button delete" type="button" aria-label={`Delete ${worker.name}`} onclick={() => actions.requestDeleteWorkers([worker.id])}>Delete</button>
          <button class="banner-button" type="button" onclick={() => actions.selectWorker(worker.id)}>Open in Mission Control</button>
        </div>
      </div>
    {/if}

    <div class="panel-body">
      <WorkerThread {worker} compact />
    </div>
  </aside>
{/if}

<style>
  .worker-panel { position: relative; display: flex; flex: 0 0 auto; min-width: 0; min-height: 0; flex-direction: column; border-left: 1px solid var(--line); background: var(--surface); }
  .resizer { position: absolute; top: 0; bottom: 0; left: -4px; z-index: 3; width: 8px; padding: 0; border: 0; background: transparent; cursor: col-resize; touch-action: none; }
  .resizer::after { position: absolute; top: 0; bottom: 0; left: 3px; width: 2px; background: transparent; content: ""; transition: background 140ms ease; }
  .resizer:hover::after, .resizer.resizing::after, .resizer:focus-visible::after { background: var(--accent); }
  .panel-header { display: grid; flex: 0 0 auto; gap: 6px; padding: 10px 12px; border-bottom: 1px solid var(--line); }
  .panel-heading { display: flex; align-items: center; gap: 10px; min-width: 0; }
  .panel-heading select { max-width: 60%; padding: 5px 8px; border: 1px solid var(--line); border-radius: 7px; background: var(--surface-soft); color: var(--ink); font-size: var(--text-body); font-weight: 600; }
  .panel-status { color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.07em; text-transform: uppercase; }
  .panel-meta { display: flex; align-items: center; gap: 6px; min-width: 0; flex-wrap: wrap; }
  .pill { padding: 3px 7px; border-radius: 999px; border: 1px solid var(--line); background: var(--surface-soft); color: var(--muted); font-size: var(--text-meta); }
  .pill.plan { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .panel-actions { display: flex; align-items: center; gap: 6px; margin-left: auto; }
  .handoff-button { display: inline-flex; align-items: center; gap: 5px; padding: 5px 9px; border: 1px solid var(--line); border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-body); font-weight: 600; }
  .handoff-button:hover { border-color: var(--line-strong); background: var(--surface-soft); color: var(--ink); }
  .close-button { display: grid; width: 28px; height: 28px; place-items: center; border: 0; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; }
  .close-button:hover { background: var(--surface-soft); color: var(--ink); }
  .panel-body { flex: 1; min-height: 0; display: flex; }
  .panel-banner { display: grid; flex: 0 0 auto; gap: 8px; padding: 10px 12px; border-bottom: 1px solid var(--line); background: var(--warning-soft); }
  .banner-error { margin: 0; color: var(--danger); font-size: var(--text-body); line-height: 1.45; }
  .banner-actions { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .banner-button { display: inline-flex; align-items: center; padding: 5px 9px; border: 1px solid var(--line-strong); border-radius: 7px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: var(--text-body); font-weight: 600; }
  .banner-button:hover { background: var(--surface-soft); color: var(--ink); }
  .banner-button.continue { border-color: var(--danger); background: var(--danger); color: var(--on-danger); }
  .banner-button.continue:hover { background: var(--danger-hover); }
  .banner-button.delete { color: var(--danger); }
  .banner-button.delete:hover { border-color: var(--danger); background: var(--surface-soft); }
  .back-button { display: none; }
  @media (max-width: 980px) {
    .worker-panel { position: absolute; inset: 0; z-index: 30; width: 100% !important; border-left: 0; box-shadow: var(--shadow); }
    .resizer { display: none; }
    .back-button { display: grid; width: 28px; height: 28px; flex: 0 0 auto; place-items: center; border: 0; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; }
    .panel-heading { flex: 1; }
  }
</style>