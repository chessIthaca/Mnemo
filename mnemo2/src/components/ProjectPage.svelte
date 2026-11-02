<script lang="ts">
  import { actions, currentProject } from "../lib/store";
  import LeadModelPicker from "./LeadModelPicker.svelte";

  /**
   * The Project page — the first page of the settings dialog, and the one the
   * top bar's gear opens. Everything here belongs to one project: the worker
   * slot cap the team runs on, the lead's model, and the merge policy this app
   * enforces. It reads the open project from the store, so it needs no
   * transport — but it does need a project, and without one it says so (the
   * dialog then opens on Endpoints instead).
   */
  const project = $derived($currentProject);

  /** The store action validates the range; a cleared or fractional box is ignored. */
  function setSlots(event: Event) {
    const slots = (event.currentTarget as HTMLInputElement).valueAsNumber;
    if (Number.isFinite(slots)) actions.setWorkerSlots(slots);
  }
</script>

<div class="project-page">
  <div class="project-scroll">
    {#if project}
      <p class="page-copy">Worker slots persist per project. Usage is telemetry, not a spending limit.</p>
      <div class="page-row">
        <div class="page-text">
          <strong>Worker slots</strong>
          <small>Maximum concurrent workers for this project.</small>
        </div>
        <input aria-label="Project worker slots" type="number" min="1" max="32" step="1" value={project.workerSlots} onchange={setSlots} />
      </div>
      <div class="page-row">
        <div class="page-text">
          <strong>Lead model</strong>
          <small>Planning and review</small>
        </div>
        <LeadModelPicker compact />
      </div>
      <div class="page-row">
        <div class="page-text">
          <strong>Merge policy</strong>
          <small>Checks and review must close on the merge-time tip; merges are serialized into this project’s working branch.</small>
        </div>
        <span class="page-value">Guarded</span>
      </div>
    {:else}
      <p class="page-empty">Open a project to change its settings.</p>
    {/if}
  </div>
</div>

<style>
  .project-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .project-scroll { display: grid; flex: 1; align-content: start; padding: 18px 24px; overflow-y: auto; }
  .page-copy { margin: 0 0 16px; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }
  .page-empty { margin: 0; color: var(--muted); font-size: var(--text-body); }
  .page-row { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 13px 0; border-top: 1px solid var(--line); }
  .page-text strong, .page-text small { display: block; }
  .page-text strong { font-size: var(--text-body); font-weight: 650; }
  .page-text small { max-width: 320px; margin-top: 3px; color: var(--muted); font-size: var(--text-meta); line-height: 1.45; }
  .page-row input { width: 54px; padding: 6px; border: 1px solid var(--line); border-radius: 7px; background: var(--surface); color: var(--ink); font-size: var(--text-body); text-align: center; }
  .page-value { flex: 0 0 auto; padding: 4px 7px; border-radius: 6px; background: var(--surface-soft); color: var(--accent-strong); font-size: var(--text-meta); }
</style>
