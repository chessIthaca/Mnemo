<script lang="ts">
  import type { LeadModel } from "../lib/types";

  let {
    models,
    spec,
    effort,
    label,
    disabled = false,
    compact = false,
    onchange,
  }: {
    models: LeadModel[];
    /** The selected `endpoint/model` spec. */
    spec: string;
    /** The selected reasoning effort; null = the model's default. */
    effort: string | null;
    /** Who the choice is for ("Lead", "Worker"); prefixes the accessible names. */
    label: string;
    disabled?: boolean;
    compact?: boolean;
    onchange: (spec: string, effort: string | null) => void;
  } = $props();

  const current = $derived(models.find((model) => model.spec === spec) ?? null);
  const efforts = $derived(current?.reasoning_efforts ?? []);

  /** The explicit name: endpoint and model, so equal model ids never collide. */
  function modelLabel(model: LeadModel): string {
    return `${model.endpoint} / ${model.model}${model.is_default ? " (default)" : ""}`;
  }

  function chooseModel(event: Event) {
    const next = (event.currentTarget as HTMLSelectElement).value;
    if (!next || next === spec) return;
    const allowed = models.find((model) => model.spec === next)?.reasoning_efforts ?? [];
    // An effort the new model does not accept falls back to its default.
    onchange(next, effort !== null && allowed.includes(effort) ? effort : null);
  }

  function chooseEffort(event: Event) {
    const value = (event.currentTarget as HTMLSelectElement).value;
    const next = value === "" ? null : value;
    if (next !== effort) onchange(spec, next);
  }
</script>

<span class="model-effort" class:compact>
  <select
    class="model-select"
    value={spec}
    {disabled}
    onchange={chooseModel}
    aria-label={`${label} model`}
    title={current ? `${current.spec}${current.is_default ? " (configured default)" : ""}` : spec}
  >
    {#each models as option (option.spec)}
      <option value={option.spec}>{modelLabel(option)}</option>
    {/each}
  </select>
  {#if efforts.length > 0}
    <select
      class="effort-select"
      value={effort ?? ""}
      {disabled}
      onchange={chooseEffort}
      aria-label={`${label} reasoning effort`}
      title="Reasoning effort"
    >
      <option value="">effort: default ({current?.default_effort ?? "model"})</option>
      {#each efforts as option (option)}
        <option value={option}>effort: {option}</option>
      {/each}
    </select>
  {/if}
</span>

<style>
  .model-effort { display: inline-flex; align-items: center; gap: 2px; min-width: 0; max-width: 100%; }
  select {
    min-width: 0;
    padding: 3px 18px 3px 5px;
    overflow: hidden;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--accent-strong);
    cursor: pointer;
    font-size: var(--text-meta);
    font-weight: 600;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .model-select { max-width: 260px; }
  .effort-select { max-width: 150px; color: var(--ink-soft); font-weight: 550; }
  select:hover:not(:disabled) { background: var(--accent-soft); }
  select:disabled { cursor: default; opacity: 0.62; }
  .compact .model-select { max-width: 210px; }
  .compact .effort-select { max-width: 120px; }
</style>
