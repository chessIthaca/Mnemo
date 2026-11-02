<script lang="ts">
  import { Plus, X } from "@lucide/svelte";
  import {
    OPTIMIZER_KNOBS,
    OPTIMIZER_LEVERS,
    modelIdOptions,
    optimizerDraft,
    optimizerWire,
    pricingDraft,
    pricingWire,
    type ModelSettings,
    type ModelSettingsTransport,
    type OptimizerDraft,
    type PricingDraftRow,
  } from "../lib/modelSettings";
  import type { EndpointDetailDto } from "../lib/settingsEngine";
  import { errorText } from "../lib/settingsState";

  /**
   * The Savings settings page: one switch per optimizer lever with a
   * one-line explanation, the knobs as small inputs, and the per-model
   * pricing table the Cost tab figures are computed from. The levers save
   * as the "optimizer" section and the prices as the "pricing" section;
   * the engine reads both when it starts, so a change applies after
   * reopening the project.
   */
  let {
    transport,
    endpoints,
  }: {
    transport: ModelSettingsTransport;
    endpoints: EndpointDetailDto[];
  } = $props();

  let loaded = $state(false);
  let loadError = $state("");

  let optimizer = $state({ draft: emptyOptimizer(), original: emptyOptimizer(), saving: false, error: "", saved: false });
  let pricing = $state({ draft: [] as PricingDraftRow[], original: [] as PricingDraftRow[], saving: false, error: "", saved: false });

  function emptyOptimizer(): OptimizerDraft {
    return {
      delta_reads: true,
      compress_output: true,
      archive: true,
      compaction_survival: true,
      quality_score: true,
      lean_output_nudge: true,
      recall_delta: true,
      archive_min_chars: "",
      compress_min_chars: "",
      lean_output_fill_pct: "",
      nudge_cooldown_requests: "",
      compress_extra_commands: "",
    };
  }

  const options = $derived(modelIdOptions(endpoints));
  const optimizerDirty = $derived(different(optimizer.draft, optimizer.original));
  const pricingDirty = $derived(different(pricing.draft, pricing.original));

  function different(a: unknown, b: unknown): boolean {
    return JSON.stringify(a) !== JSON.stringify(b);
  }

  $effect(() => {
    let cancelled = false;
    loadError = "";
    transport
      .get()
      .then((settings: ModelSettings) => {
        if (cancelled) return;
        optimizer.draft = optimizerDraft(settings.optimizer);
        optimizer.original = optimizerDraft(settings.optimizer);
        pricing.draft = pricingDraft(settings.pricing);
        pricing.original = pricingDraft(settings.pricing);
        loaded = true;
      })
      .catch((failure) => {
        if (!cancelled) loadError = errorText(failure);
      });
    return () => {
      cancelled = true;
    };
  });

  async function saveLevers(): Promise<void> {
    const parsed = optimizerWire(optimizer.draft);
    if (parsed.error) {
      optimizer.error = parsed.error;
      return;
    }
    optimizer.saving = true;
    optimizer.error = "";
    try {
      await transport.save("optimizer", parsed.value);
      optimizer.original = { ...optimizer.draft };
      optimizer.saved = true;
    } catch (failure) {
      optimizer.error = errorText(failure);
    } finally {
      optimizer.saving = false;
    }
  }

  async function savePrices(): Promise<void> {
    const parsed = pricingWire(pricing.draft);
    if (parsed.error) {
      pricing.error = parsed.error;
      return;
    }
    pricing.saving = true;
    pricing.error = "";
    try {
      await transport.save("pricing", parsed.value);
      pricing.original = pricing.draft.map((row) => ({ ...row }));
      pricing.saved = true;
    } catch (failure) {
      pricing.error = errorText(failure);
    } finally {
      pricing.saving = false;
    }
  }

  function discardLevers(): void {
    optimizer.draft = { ...optimizer.original };
    optimizer.error = "";
    optimizer.saved = false;
  }

  function discardPrices(): void {
    pricing.draft = pricing.original.map((row) => ({ ...row }));
    pricing.error = "";
    pricing.saved = false;
  }

  function addRow(): void {
    pricing.draft.push({ model: "", input: "0", output: "0", cached: "0" });
  }

  function removeRow(index: number): void {
    pricing.draft.splice(index, 1);
  }
</script>

<datalist id="savings-page-models">
  {#each options as option (option)}
    <option value={option}></option>
  {/each}
</datalist>

<div class="savings-page">
  <div class="savings-scroll">
    {#if loadError}
      <p class="load-error" role="alert">{loadError}</p>
    {:else if !loaded}
      <p class="load-note">Loading…</p>
    {:else}
      <section class="group" aria-label="Savings levers">
        <header>
          <h3>Savings levers</h3>
          <p class="micro">
            The token savings the engine applies by default. Turn one off only when you want the full, unfiltered context.
          </p>
        </header>
        <div class="lever-list">
          {#each OPTIMIZER_LEVERS as lever (lever.key)}
            <label class="lever-row">
              <input type="checkbox" bind:checked={optimizer.draft[lever.key]} />
              <span class="lever-text">
                <strong>{lever.label}</strong>
                <span class="micro">{lever.explanation}</span>
              </span>
            </label>
          {/each}
        </div>
        <div class="knob-list">
          {#each OPTIMIZER_KNOBS as knob (knob.key)}
            <div class="knob-row">
              <label for={`knob-${knob.key}`}>{knob.label}</label>
              <span class="micro">{knob.explanation}</span>
              <input
                id={`knob-${knob.key}`}
                inputmode={knob.isNumber ? "numeric" : "text"}
                bind:value={optimizer.draft[knob.key]}
                spellcheck="false"
              />
            </div>
          {/each}
        </div>
        <div class="section-actions">
          {#if optimizer.error}<p class="section-error" role="alert">{optimizer.error}</p>{/if}
          {#if optimizer.saved && !optimizerDirty}<p class="section-note">Applies after reopening the project.</p>{/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={optimizer.saving || !optimizerDirty} onclick={discardLevers}>Discard</button>
            <button class="button button-primary" disabled={optimizer.saving || !optimizerDirty} onclick={saveLevers}>
              {optimizer.saving ? "Saving…" : "Save levers"}
            </button>
          </div>
        </div>
      </section>

      <section class="group" aria-label="Per-model pricing">
        <header>
          <h3>Per-model pricing</h3>
          <p class="micro">US dollars per 1M tokens. The Cost tab computes its figures from these prices.</p>
        </header>
        <table class="pricing-table">
          <thead>
            <tr><th>Model</th><th>Input $/1M</th><th>Output $/1M</th><th>Cached $/1M</th><th></th></tr>
          </thead>
          <tbody>
            {#each pricing.draft as row, index}
              <tr>
                <td>
                  <input list="savings-page-models" aria-label={`Model for row ${index + 1}`} bind:value={row.model} placeholder="model id" spellcheck="false" />
                </td>
                <td><input inputmode="decimal" aria-label={`Input price for row ${index + 1}`} bind:value={row.input} /></td>
                <td><input inputmode="decimal" aria-label={`Output price for row ${index + 1}`} bind:value={row.output} /></td>
                <td><input inputmode="decimal" aria-label={`Cached price for row ${index + 1}`} bind:value={row.cached} /></td>
                <td>
                  <button class="icon-button" aria-label={`Remove row ${index + 1}`} onclick={() => removeRow(index)}>
                    <X size={12} />
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
        <div>
          <button class="button button-quiet" onclick={addRow}><Plus size={13} /> Add model</button>
        </div>
        <div class="section-actions">
          {#if pricing.error}<p class="section-error" role="alert">{pricing.error}</p>{/if}
          {#if pricing.saved && !pricingDirty}<p class="section-note">Applies after reopening the project.</p>{/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={pricing.saving || !pricingDirty} onclick={discardPrices}>Discard</button>
            <button class="button button-primary" disabled={pricing.saving || !pricingDirty} onclick={savePrices}>
              {pricing.saving ? "Saving…" : "Save prices"}
            </button>
          </div>
        </div>
      </section>
    {/if}
  </div>
</div>

<style>
  .savings-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .savings-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }
  .load-error { margin: 0; padding: 18px 24px; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  .load-note { margin: 0; padding: 18px 24px; color: var(--muted); font-size: var(--text-body); }

  .group {
    display: grid;
    gap: 12px;
    padding: 16px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--surface);
  }
  .group header { display: grid; gap: 4px; }
  .group h3 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .micro { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }

  .lever-list { display: grid; gap: 4px; }
  .lever-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    cursor: pointer;
    font-size: var(--text-body);
  }
  .lever-row:has(input:checked) { border-color: var(--accent); background: var(--accent-soft); }
  .lever-text { display: grid; gap: 1px; }
  .lever-text strong { font-weight: 620; }

  .knob-list { display: grid; grid-template-columns: 1fr 1fr; gap: 10px 18px; }
  .knob-row { display: grid; gap: 3px; }
  .knob-row label { font-size: var(--text-body); font-weight: 620; }
  .knob-row input {
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface);
    font-size: var(--text-body);
  }

  .pricing-table { border-collapse: collapse; font-size: var(--text-body); }
  .pricing-table th {
    padding: 4px 8px;
    color: var(--muted);
    font-size: var(--text-meta);
    font-weight: 650;
    text-align: left;
    white-space: nowrap;
  }
  .pricing-table td { padding: 3px 4px 3px 0; }
  .pricing-table input {
    width: 100%;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface);
    font-size: var(--text-body);
  }

  .section-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 2px; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; flex: 1; }
  .section-note { margin: 0; color: var(--muted); font-size: var(--text-body); flex: 1; }
  .section-buttons { display: flex; gap: 8px; flex-shrink: 0; }
</style>