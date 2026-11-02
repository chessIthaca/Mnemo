<script lang="ts">
  import { Download } from "@lucide/svelte";
  import { EMBEDDING_KEYWORD_ID, EMBEDDING_STATUS_META, budgetDraft, budgetWire, isValidModelRef, modelRefOptions, TASK_SLOTS, type BudgetDraft, type EmbeddingSection, type ModelSettingsTransport, type TaskModels } from "../lib/modelSettings";
  import type { EndpointDetailDto } from "../lib/settingsEngine";
  import { errorText } from "../lib/settingsState";

  /**
   * The Models settings page: the model each kind of work uses beyond the
   * tiers (planning, executing, bug fixing, reviewing, summarize), the
   * vision model, the code-indexing model with its download status, and
   * the budget caps. Every section saves on its own button and only writes
   * its own config section; the engine reads the config on start, so a
   * change applies after reopening the project.
   */
  let {
    transport,
    endpoints,
  }: {
    transport: ModelSettingsTransport;
    endpoints: EndpointDetailDto[];
  } = $props();

  /** The embedder section of the snapshot: live status plus the catalog. */
  let embedder = $state<EmbeddingSection | null>(null);
  let loadError = $state("");

  let tasks = $state({ draft: emptyTasks(), original: emptyTasks(), saving: false, error: "", saved: false });
  let vision = $state({ draft: "", original: "", saving: false, error: "", saved: false });
  let index = $state({ draft: "", original: "", saving: false, error: "", saved: false });
  let budget = $state({ draft: emptyBudget(), original: emptyBudget(), saving: false, error: "", saved: false });

  let installBusy = $state(false);
  let installProgress = $state<{ label: string; fraction: number } | null>(null);
  let installError = $state("");

  function emptyTasks(): TaskModels {
    return { planning: null, executing: null, bug_fixing: null, reviewing: null, summarize: null };
  }

  function emptyBudget(): BudgetDraft {
    return { enabled: false, max_tokens_per_plan: "", max_escalations_per_plan: "", max_retries_per_lane: "" };
  }

  const options = $derived(modelRefOptions(endpoints));
  const tasksDirty = $derived(different(tasks.draft, tasks.original));
  const visionDirty = $derived(vision.draft !== vision.original);
  const indexDirty = $derived(index.draft !== index.original);
  const budgetDirty = $derived(different(budget.draft, budget.original));

  function different(a: unknown, b: unknown): boolean {
    return JSON.stringify(a) !== JSON.stringify(b);
  }

  /** The drafted code-indexing choice, for the Install button and its label. */
  const draftOption = $derived(embedder?.options.find((option) => option.id === index.draft) ?? null);
  const installable = $derived(draftOption !== null && !draftOption.installed && draftOption.id !== EMBEDDING_KEYWORD_ID);

  $effect(() => {
    let cancelled = false;
    loadError = "";
    transport
      .get()
      .then((settings) => {
        if (cancelled) return;
        embedder = settings.embedding;
        tasks.draft = { ...settings.task_models };
        tasks.original = { ...settings.task_models };
        vision.draft = settings.vision_model ?? "";
        vision.original = settings.vision_model ?? "";
        index.draft = settings.embedding.selected;
        index.original = settings.embedding.selected;
        budget.draft = budgetDraft(settings.budget);
        budget.original = budgetDraft(settings.budget);
      })
      .catch((failure) => {
        if (!cancelled) loadError = errorText(failure);
      });
    return () => {
      cancelled = true;
    };
  });

  /** Follow the install's progress events so the bar and status stay honest. */
  $effect(() => {
    let cancelled = false;
    let stop: (() => void) | null = null;
    void transport
      .onEmbedderEvent((event) => {
        if (event.step !== "embedder_model") return;
        if (event.type === "progress") {
          installBusy = true;
          installError = "";
          installProgress = { label: event.label ?? "", fraction: event.fraction ?? 0 };
        } else if (event.type === "done") {
          installBusy = false;
          installProgress = null;
          void refreshEmbedder();
        } else {
          installBusy = false;
          installProgress = null;
          installError = event.error ?? "Download failed";
          void refreshEmbedder();
        }
      })
      .then((unlisten) => {
        if (cancelled) unlisten();
        else stop = unlisten;
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      stop?.();
      stop = null;
    };
  });

  async function refreshEmbedder(): Promise<void> {
    try {
      embedder = (await transport.get()).embedding;
    } catch {
      // The status line just stays as it was; the next open refreshes it.
    }
  }

  async function saveTasks(): Promise<void> {
    const value: TaskModels = {
      planning: tasks.draft.planning?.trim() || null,
      executing: tasks.draft.executing?.trim() || null,
      bug_fixing: tasks.draft.bug_fixing?.trim() || null,
      reviewing: tasks.draft.reviewing?.trim() || null,
      summarize: tasks.draft.summarize?.trim() || null,
    };
    const invalid = (Object.keys(value) as (keyof TaskModels)[]).find((key) => value[key] !== null && !isValidModelRef(value[key]!));
    if (invalid) {
      const slot = TASK_SLOTS.find((entry) => entry.key === invalid)!;
      tasks.error = `${slot.label} model must be "endpoint/model"`;
      return;
    }
    await persist(tasks, "models", value, () => {
      tasks.draft = { ...value };
      tasks.original = { ...value };
    });
  }

  async function saveVision(): Promise<void> {
    const value = vision.draft.trim();
    if (value !== "" && !isValidModelRef(value)) {
      vision.error = 'Vision model must be "endpoint/model"';
      return;
    }
    await persist(vision, "vision", value === "" ? null : value, () => {
      vision.draft = value;
      vision.original = value;
    });
  }

  async function saveIndex(): Promise<void> {
    await persist(index, "embedding", index.draft, () => (index.original = index.draft));
  }

  async function saveBudget(): Promise<void> {
    const parsed = budgetWire(budget.draft);
    if (parsed.error) {
      budget.error = parsed.error;
      return;
    }
    await persist(budget, "budget", parsed.value, () => (budget.original = { ...budget.draft }));
  }

  /** One section save: error and success states are the section's own. */
  async function persist(
    section: { saving: boolean; error: string; saved: boolean },
    name: "models" | "vision" | "embedding" | "budget",
    value: unknown,
    accept: () => void,
  ): Promise<void> {
    section.saving = true;
    section.error = "";
    try {
      await transport.save(name, value);
      accept();
      section.saved = true;
    } catch (failure) {
      section.error = errorText(failure);
    } finally {
      section.saving = false;
    }
  }

  async function install(): Promise<void> {
    if (!draftOption) return;
    installError = "";
    installBusy = true;
    try {
      await transport.install(draftOption.id);
    } catch (failure) {
      installBusy = false;
      installError = errorText(failure);
    }
  }

  function discardTasks(): void {
    tasks.draft = { ...tasks.original };
    tasks.error = "";
    tasks.saved = false;
  }

  function discardVision(): void {
    vision.draft = vision.original;
    vision.error = "";
    vision.saved = false;
  }

  function discardIndex(): void {
    index.draft = index.original;
    index.error = "";
    index.saved = false;
  }

  function discardBudget(): void {
    budget.draft = { ...budget.original };
    budget.error = "";
    budget.saved = false;
  }
</script>

<datalist id="models-page-refs">
  {#each options as option (option)}
    <option value={option}></option>
  {/each}
</datalist>

<div class="models-page">
  <div class="models-scroll">
    {#if loadError}
      <p class="load-error" role="alert">{loadError}</p>
    {:else if embedder === null}
      <p class="load-note">Loading…</p>
    {:else}
      <section class="group" aria-label="Models for each kind of work">
        <header>
          <h3>Models for each kind of work</h3>
          <p class="micro">
            Overrides the tier model for one kind of work, as "endpoint/model". Empty fields follow the tier the lead picks.
          </p>
        </header>
        {#each TASK_SLOTS as slot (slot.key)}
          <div class="slot-row">
            <div class="slot-heading">
              <label for={`slot-${slot.key}`}>{slot.label}</label>
              <span class="micro">{slot.blurb}</span>
            </div>
            <div class="slot-input">
              <input
                id={`slot-${slot.key}`}
                list="models-page-refs"
                bind:value={tasks.draft[slot.key]}
                placeholder="endpoint/model"
                spellcheck="false"
              />
              <button
                class="button button-quiet"
                aria-label={`Use the default for ${slot.label}`}
                disabled={tasks.draft[slot.key] === null || tasks.draft[slot.key] === ""}
                onclick={() => (tasks.draft[slot.key] = "")}
              >
                Default
              </button>
            </div>
          </div>
        {/each}
        <div class="section-actions">
          {#if tasks.error}<p class="section-error" role="alert">{tasks.error}</p>{/if}
          {#if tasks.saved && !tasksDirty}<p class="section-note">Applies after reopening the project.</p>{/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={tasks.saving || !tasksDirty} onclick={discardTasks}>Discard</button>
            <button class="button button-primary" disabled={tasks.saving || !tasksDirty} onclick={saveTasks}>
              {tasks.saving ? "Saving…" : "Save models"}
            </button>
          </div>
        </div>
      </section>

      <section class="group" aria-label="Vision">
        <header>
          <h3>Vision</h3>
          <p class="micro">The model that reads images. Empty uses the multimodal main model serving the turn.</p>
        </header>
        <div class="slot-input">
          <input
            list="models-page-refs"
            aria-label="Vision model"
            bind:value={vision.draft}
            placeholder="endpoint/model"
            spellcheck="false"
          />
          <button
            class="button button-quiet"
            aria-label="Use the default for Vision"
            disabled={vision.draft === ""}
            onclick={() => (vision.draft = "")}
          >
            Default
          </button>
        </div>
        <div class="section-actions">
          {#if vision.error}<p class="section-error" role="alert">{vision.error}</p>{/if}
          {#if vision.saved && !visionDirty}<p class="section-note">Applies after reopening the project.</p>{/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={vision.saving || !visionDirty} onclick={discardVision}>Discard</button>
            <button class="button button-primary" disabled={vision.saving || !visionDirty} onclick={saveVision}>
              {vision.saving ? "Saving…" : "Save vision"}
            </button>
          </div>
        </div>
      </section>

      <section class="group" aria-label="Code indexing">
        <header>
          <h3>Code indexing</h3>
          <p class="micro">
            The model that embeds your code for recall. "Keyword only" skips the model and matches on keywords instead.
          </p>
        </header>
        <div class="embed-list" role="radiogroup" aria-label="Code indexing model">
          {#each embedder.options as option (option.id)}
            <label class="embed-option">
              <input type="radio" name="code-indexing-model" value={option.id} bind:group={index.draft} />
              <span class="embed-name">{option.label}</span>
              {#if option.size_mb > 0}<span class="embed-size">{option.size_mb} MB</span>{/if}
              {#if option.installed && option.id !== EMBEDDING_KEYWORD_ID}<span class="embed-badge">Installed</span>{/if}
            </label>
          {/each}
        </div>
        <div class="embed-status">
          <span class="embed-state">Status: {EMBEDDING_STATUS_META[embedder.status]}</span>
          {#if draftOption && installable}
            <button class="button button-quiet" disabled={installBusy} onclick={install}>
              <Download size={13} />
              {installBusy ? "Downloading…" : "Install"}
            </button>
          {/if}
        </div>
        {#if installProgress}
          <div
            class="progress"
            role="progressbar"
            aria-label="Download progress"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(installProgress.fraction * 100)}
          >
            <div style="width: {Math.min(100, Math.round(installProgress.fraction * 100))}%"></div>
          </div>
          <p class="micro progress-label">{installProgress.label}</p>
        {/if}
        {#if installError}<p class="section-error" role="alert">{installError}</p>{/if}
        <div class="section-actions">
          {#if index.error}<p class="section-error" role="alert">{index.error}</p>{/if}
          {#if index.saved && !indexDirty}
            <p class="section-note">Applies after reopening the project — a fresh download is used from the next reopen.</p>
          {/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={index.saving || !indexDirty} onclick={discardIndex}>Discard</button>
            <button class="button button-primary" disabled={index.saving || !indexDirty} onclick={saveIndex}>
              {index.saving ? "Saving…" : "Save indexing"}
            </button>
          </div>
        </div>
      </section>

      <section class="group" aria-label="Budget">
        <header>
          <h3>Budget</h3>
          <p class="micro">Deterministic caps the engine refuses to cross. Turn them off only when you accept the runaway costs.</p>
        </header>
        <label class="budget-toggle">
          <input type="checkbox" bind:checked={budget.draft.enabled} />
          <span>Enforce budget caps</span>
        </label>
        <div class="budget-grid">
          <label for="budget-tokens">Tokens per plan</label>
          <input id="budget-tokens" inputmode="numeric" bind:value={budget.draft.max_tokens_per_plan} spellcheck="false" />
          <label for="budget-escalations">Escalations per plan</label>
          <input id="budget-escalations" inputmode="numeric" bind:value={budget.draft.max_escalations_per_plan} spellcheck="false" />
          <label for="budget-retries">Retries per lane</label>
          <input id="budget-retries" inputmode="numeric" bind:value={budget.draft.max_retries_per_lane} spellcheck="false" />
        </div>
        <div class="section-actions">
          {#if budget.error}<p class="section-error" role="alert">{budget.error}</p>{/if}
          {#if budget.saved && !budgetDirty}<p class="section-note">Applies after reopening the project.</p>{/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={budget.saving || !budgetDirty} onclick={discardBudget}>Discard</button>
            <button class="button button-primary" disabled={budget.saving || !budgetDirty} onclick={saveBudget}>
              {budget.saving ? "Saving…" : "Save budget"}
            </button>
          </div>
        </div>
      </section>
    {/if}
  </div>
</div>

<style>
  .models-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .models-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }
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

  .slot-row { display: grid; gap: 6px; }
  .slot-heading { display: flex; align-items: baseline; gap: 8px; }
  .slot-heading label { font-size: var(--text-body); font-weight: 620; }
  .slot-input { display: flex; gap: 8px; }
  .slot-input input { flex: 1; min-width: 0; padding: 7px 10px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); font-size: var(--text-body); }
  .slot-input .button { flex-shrink: 0; min-height: 32px; padding: 5px 10px; }

  .embed-list { display: grid; gap: 4px; }
  .embed-option {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    cursor: pointer;
    font-size: var(--text-body);
  }
  .embed-option:has(input:checked) { border-color: var(--accent); background: var(--accent-soft); }
  .embed-name { font-weight: 560; }
  .embed-size { color: var(--muted); }
  .embed-badge {
    padding: 1px 7px;
    border-radius: 999px;
    background: var(--accent-soft);
    color: var(--accent-strong);
    font-size: var(--text-meta);
    font-weight: 650;
  }
  .embed-status { display: flex; align-items: center; gap: 12px; }
  .embed-state { font-size: var(--text-body); color: var(--ink-soft); }

  .progress { height: 6px; border-radius: 999px; background: var(--surface-soft); overflow: hidden; }
  .progress > div { height: 100%; border-radius: inherit; background: var(--accent); transition: width 0.2s ease; }
  .progress-label { margin-top: -8px; }

  .budget-toggle { display: flex; align-items: center; gap: 10px; font-size: var(--text-body); }
  .budget-grid { display: grid; grid-template-columns: auto 1fr; gap: 8px 12px; align-items: center; }
  .budget-grid label { font-size: var(--text-body); color: var(--ink-soft); }
  .budget-grid input {
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