<script lang="ts">
  import { actions, app, currentProject } from "../lib/store";
  import ModelBadge from "./ModelBadge.svelte";
  import ModelEffortPicker from "./ModelEffortPicker.svelte";

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

  function choose(spec: string, effort: string | null) {
    if (spec !== runtime.model || effort !== runtime.reasoningEffort) {
      actions.setLeadModel(spec, effort);
    }
  }
</script>

{#if runtime.mode === "native" && runtime.models.length > 0}
  <span class="lead-model-picker">
    <ModelEffortPicker
      models={runtime.models}
      spec={selected}
      effort={runtime.reasoningEffort}
      label="Lead"
      disabled={runtime.busy || runtime.pending !== null}
      {compact}
      onchange={choose}
    />
  </span>
{:else}
  <ModelBadge model={label} role="lead" />
{/if}

<style>
  .lead-model-picker { display: inline-flex; min-width: 0; max-width: 100%; }
</style>
