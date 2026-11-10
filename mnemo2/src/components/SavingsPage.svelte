<script lang="ts">
  import {
    OPTIMIZER_KNOBS,
    OPTIMIZER_LEVERS,
    optimizerDraft,
    optimizerWire,
    type ModelSettings,
    type ModelSettingsTransport,
    type OptimizerDraft,
  } from "../lib/modelSettings";
  import { errorText } from "../lib/settingsState";
  import { trackUnsaved } from "../lib/unsavedEdits";

  /**
   * The Savings settings page: one switch per optimizer lever with a
   * one-line explanation, plus the knobs as small inputs. The levers save
   * as the "optimizer" section, which the engine reads when it starts, so
   * a change applies after reopening the project. Model prices are not set
   * here — each model carries its own prices on the Endpoints page.
   */
  let { transport }: { transport: ModelSettingsTransport } = $props();

  let loaded = $state(false);
  let loadError = $state("");

  let optimizer = $state({ draft: emptyOptimizer(), original: emptyOptimizer(), saving: false, error: "", saved: false });

  function emptyOptimizer(): OptimizerDraft {
    return {
      delta_reads: true,
      compress_output: true,
      archive: true,
      compaction_survival: true,
      quality_score: true,
      lean_output_nudge: true,
      recall_delta: true,
      idle_compaction: true,
      archive_min_chars: "",
      compress_min_chars: "",
      lean_output_fill_pct: "",
      nudge_cooldown_requests: "",
      compress_extra_commands: "",
    };
  }

  const optimizerDirty = $derived(different(optimizer.draft, optimizer.original));
  trackUnsaved(() => optimizerDirty);

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

  function discardLevers(): void {
    optimizer.draft = { ...optimizer.original };
    optimizer.error = "";
    optimizer.saved = false;
  }
</script>

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
        <p class="micro">Model prices are set with each model on the Endpoints page.</p>
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

  .section-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 2px; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; flex: 1; }
  .section-note { margin: 0; color: var(--muted); font-size: var(--text-body); flex: 1; }
  .section-buttons { display: flex; gap: 8px; flex-shrink: 0; }
</style>