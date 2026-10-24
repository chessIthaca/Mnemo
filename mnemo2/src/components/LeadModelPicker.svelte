<script lang="ts">
  import { actions, app, currentProject } from "../lib/store";
  import ModelBadge from "./ModelBadge.svelte";

  let { compact = false }: { compact?: boolean } = $props();

  const runtime = $derived($app.leadRuntime);
  const fallbackModel = $derived($currentProject?.leadModel ?? "unconfigured");
  const selected = $derived(
    runtime.model ??
      runtime.models.find((model) => model.is_default)?.spec ??
      runtime.models[0]?.spec ??
      "",
  );
  const label = $derived(runtime.modelId ?? fallbackModel);

  function choose(event: Event) {
    const spec = (event.currentTarget as HTMLSelectElement).value;
    if (spec && spec !== runtime.model) actions.setLeadModel(spec);
  }
</script>

{#if runtime.mode === "native" && runtime.models.length > 0}
  <label class="lead-model-picker" class:compact>
    <span class="sr-only">Lead model</span>
    <select
      value={selected}
      disabled={runtime.busy || runtime.pending !== null}
      onchange={choose}
      aria-label="Lead model"
      title={label}
    >
      {#each runtime.models as option}
        <option value={option.spec}>{option.model}</option>
      {/each}
    </select>
  </label>
{:else}
  <ModelBadge model={label} role="lead" />
{/if}

<style>
  .lead-model-picker { display: inline-flex; min-width: 0; max-width: 100%; }
  select {
    max-width: 190px;
    padding: 3px 18px 3px 5px;
    overflow: hidden;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--accent-strong);
    cursor: pointer;
    font-size: 9px;
    font-weight: 600;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  select:hover:not(:disabled) { background: var(--accent-soft); }
  select:disabled { cursor: default; opacity: 0.62; }
  .compact select { max-width: 150px; }
</style>
