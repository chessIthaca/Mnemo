<script lang="ts">
  import { currentProject } from "../lib/store";
  import { errorText } from "../lib/settingsState";
  import { trackUnsaved } from "../lib/unsavedEdits";
  import {
    ORIGIN_META,
    artifactDirty,
    capabilityLine,
    planSectionPreview,
    pendingPlanLine,
    schemaBy,
    validateLine,
    type ArtifactView,
    type ArtifactsView,
    type PlanPreview,
    type SchemaList,
    type SchemaSummary,
    type SpecsConfig,
    type SpecsStatus,
    type SpecsTransport,
    type ValidateOutput,
  } from "../lib/specsSettings";

  /**
   * The Specs settings page (per project): the OpenSpec schemas the plan
   * board can run on — the schema list with its active pick, adding a
   * schema from a folder or git URL, forking one (the only way to edit
   * the read-only built-in), the artifact template + instruction editor,
   * the project context and rules from `config.yaml`, the plan preview,
   * and the specs status panel with the optional `openspec validate` run.
   *
   * Everything here lives under the open project's `.coding/openspec/`;
   * without an open project the page says so instead of erroring.
   */
  let { transport }: { transport: SpecsTransport } = $props();

  /** One artifact-keyed rules row: lines are one rule per line. */
  interface RuleRow {
    key: string;
    text: string;
  }

  // ── Schema list ─────────────────────────────────────────────────────
  let schemas = $state<SchemaList | null>(null);
  let loadError = $state("");
  let activateError = $state("");
  let activatingName = $state("");

  let addSource = $state("");
  let addBusy = $state(false);
  let addError = $state("");
  let addNote = $state("");

  let forkFrom = $state("");
  let forkTo = $state("");
  let forkBusy = $state(false);
  let forkError = $state("");

  /** The schemas the pickers offer: loadable ones only. */
  const pickable = $derived((schemas?.schemas ?? []).filter((schema) => !schema.error));

  // ── Artifact editor ────────────────────────────────────────────────
  let artifactSchema = $state("");
  let artifactsView = $state<ArtifactsView | null>(null);
  let selectedArtifactId = $state("");
  let templateDraft = $state("");
  let instructionDraft = $state("");
  let artifactBusy = $state(false);
  let artifactError = $state("");
  let artifactNote = $state("");

  const selectedArtifact = $derived(
    artifactsView?.artifacts.find((artifact) => artifact.id === selectedArtifactId) ?? null,
  );
  const artifactEditable = $derived(artifactsView?.origin === "project");
  const artifactDirtyNow = $derived(
    selectedArtifact ? artifactDirty(selectedArtifact, templateDraft, instructionDraft) : false,
  );
  const templatePreview = $derived(planSectionPreview(selectedArtifactId, templateDraft));

  // ── Project context and rules ───────────────────────────────────────
  let config = $state<SpecsConfig | null>(null);
  let contextDraft = $state("");
  let ruleRows = $state<RuleRow[]>([]);
  let ruleRowsOriginal = $state<RuleRow[]>([]);
  let configBusy = $state(false);
  let configError = $state("");
  let configSaved = $state(false);

  const configDirty = $derived(
    config !== null &&
      (config.context !== contextDraft || JSON.stringify(ruleRows) !== JSON.stringify(ruleRowsOriginal)),
  );
  trackUnsaved(() => artifactDirtyNow || configDirty);

  // ── Plan preview ────────────────────────────────────────────────────
  let previewName = $state("");
  let preview = $state<PlanPreview | null>(null);
  let previewError = $state("");

  // ── Specs status ────────────────────────────────────────────────────
  let status = $state<SpecsStatus | null>(null);
  let validateBusy = $state(false);
  let validateError = $state("");
  let validateOutput = $state<ValidateOutput | null>(null);

  $effect(() => {
    // The only tracked read: reload everything when the transport changes.
    const active = transport;
    let cancelled = false;
    loadError = "";
    active
      .listSchemas()
      .then((list) => {
        if (!cancelled) acceptList(list);
      })
      .catch((failure) => {
        if (!cancelled) loadError = errorText(failure);
      });
    active
      .readConfig()
      .then((loaded) => {
        if (!cancelled) acceptConfig(loaded);
      })
      .catch((failure) => {
        if (!cancelled) configError = errorText(failure);
      });
    active
      .status()
      .then((loaded) => {
        if (!cancelled) status = loaded;
      })
      .catch((failure) => {
        if (!cancelled) loadError = loadError || errorText(failure);
      });
    return () => {
      cancelled = true;
    };
  });

  /** Adopt a schema list; the editors keep picks that still exist. */
  function acceptList(list: SchemaList): void {
    schemas = list;
    activateError = "";
    if (!pickable.some((schema) => schema.name === artifactSchema)) {
      artifactSchema = pickable.some((schema) => schema.name === list.active)
        ? list.active
        : (pickable[0]?.name ?? "");
      if (artifactSchema) void loadArtifacts(artifactSchema);
    }
    if (!pickable.some((schema) => schema.name === previewName)) {
      previewName = artifactSchema || list.active;
      void loadPreview(previewName);
    }
  }

  /** Adopt the config; a dirty draft keeps its unsaved edits. */
  function acceptConfig(loaded: SpecsConfig): void {
    config = loaded;
    configError = "";
    if (!configDirty) {
      contextDraft = loaded.context;
      ruleRows = rowsFromRules(loaded.rules);
      ruleRowsOriginal = rowsFromRules(loaded.rules);
    }
  }

  function rowsFromRules(rules: Record<string, string[]>): RuleRow[] {
    return Object.entries(rules).map(([key, values]) => ({ key, text: values.join("\n") }));
  }

  function rulesFromRows(rows: RuleRow[]): Record<string, string[]> {
    const rules: Record<string, string[]> = {};
    for (const row of rows) {
      const key = row.key.trim();
      if (!key) continue;
      const values = row.text
        .split("\n")
        .map((line) => line.trim())
        .filter((line) => line.length > 0);
      if (values.length === 0) continue;
      rules[key] = [...(rules[key] ?? []), ...values];
    }
    return rules;
  }

  // ── Actions ────────────────────────────────────────────────────────

  async function activate(name: string): Promise<void> {
    activateError = "";
    activatingName = name;
    try {
      acceptList(await transport.setActiveSchema(name));
    } catch (failure) {
      activateError = errorText(failure);
    } finally {
      activatingName = "";
    }
  }

  async function addSchema(): Promise<void> {
    addError = "";
    addNote = "";
    if (!addSource.trim()) {
      addError = "Enter a schema folder or git URL.";
      return;
    }
    addBusy = true;
    try {
      acceptList(await transport.addSchema(addSource.trim()));
      addSource = "";
      addNote = "Added.";
    } catch (failure) {
      addError = errorText(failure);
    } finally {
      addBusy = false;
    }
  }

  async function forkSchema(): Promise<void> {
    forkError = "";
    const name = forkTo.trim();
    if (!name) {
      forkError = "Enter the new schema name.";
      return;
    }
    forkBusy = true;
    try {
      const from = forkFrom.trim() || artifactSchema || schemas?.active || "spec-driven";
      acceptList(await transport.forkSchema(from, name));
      forkTo = "";
      artifactSchema = name;
      await loadArtifacts(name);
    } catch (failure) {
      forkError = errorText(failure);
    } finally {
      forkBusy = false;
    }
  }

  async function loadArtifacts(name: string): Promise<void> {
    if (!name) {
      artifactsView = null;
      selectedArtifactId = "";
      return;
    }
    artifactError = "";
    artifactNote = "";
    artifactBusy = true;
    try {
      const view = await transport.readArtifacts(name);
      artifactsView = view;
      if (!view.artifacts.some((artifact) => artifact.id === selectedArtifactId)) {
        selectedArtifactId = view.artifacts[0]?.id ?? "";
      }
      syncDrafts();
    } catch (failure) {
      artifactsView = null;
      artifactError = errorText(failure);
    } finally {
      artifactBusy = false;
    }
  }

  /** Point the editor's drafts at the selected artifact. */
  function syncDrafts(): void {
    const artifact = selectedArtifact as ArtifactView | null;
    templateDraft = artifact?.template_text ?? "";
    instructionDraft = artifact?.instruction ?? "";
    artifactNote = "";
  }

  async function saveArtifact(): Promise<void> {
    if (!artifactsView || !selectedArtifact) return;
    artifactError = "";
    artifactNote = "";
    artifactBusy = true;
    try {
      const view = await transport.saveArtifact(
        artifactsView.name,
        selectedArtifact.id,
        templateDraft,
        instructionDraft,
      );
      artifactsView = view;
      artifactNote = "Saved. The next plan brief under this schema follows it.";
    } catch (failure) {
      artifactError = errorText(failure);
    } finally {
      artifactBusy = false;
    }
  }

  function discardArtifact(): void {
    syncDrafts();
    artifactError = "";
  }

  async function saveConfig(): Promise<void> {
    configError = "";
    configSaved = false;
    configBusy = true;
    try {
      const loaded = await transport.saveConfig(contextDraft, rulesFromRows(ruleRows));
      acceptConfig(loaded);
      configSaved = true;
    } catch (failure) {
      configError = errorText(failure);
    } finally {
      configBusy = false;
    }
  }

  function discardConfig(): void {
    if (!config) return;
    contextDraft = config.context;
    ruleRows = rowsFromRules(config.rules);
    configError = "";
    configSaved = false;
  }

  function addRuleRow(): void {
    ruleRows = [...ruleRows, { key: "", text: "" }];
    configSaved = false;
  }

  function removeRuleRow(index: number): void {
    ruleRows = ruleRows.filter((_, position) => position !== index);
    configSaved = false;
  }

  async function loadPreview(name: string): Promise<void> {
    if (!name) {
      preview = null;
      previewError = "";
      return;
    }
    previewError = "";
    try {
      preview = await transport.planPreview(name);
    } catch (failure) {
      preview = null;
      previewError = errorText(failure);
    }
  }

  async function runValidate(): Promise<void> {
    validateError = "";
    validateOutput = null;
    validateBusy = true;
    try {
      validateOutput = await transport.runValidate();
    } catch (failure) {
      validateError = errorText(failure);
    } finally {
      validateBusy = false;
    }
  }

  /** One schema row's origin hint (unknown origins get a generic one). */
  function originMeta(schema: SchemaSummary): { label: string; hint: string } {
    return ORIGIN_META[schema.origin] ?? { label: schema.origin, hint: "" };
  }
</script>

<div class="specs-page">
  <div class="specs-scroll">
    {#if $currentProject === null}
      <p class="micro empty">
        Open a project to manage its specs — they live in the project's <code>.coding/openspec/</code>.
      </p>
    {:else if loadError}
      <p class="load-error" role="alert">{loadError}</p>
    {:else if schemas === null}
      <p class="load-note">Loading…</p>
    {:else}
      <section class="group" aria-label="Schemas">
        <header>
          <h3>Schemas</h3>
          <p class="micro">
            The schema a plan runs on: which sections it must have and what its workers receive. The
            active one applies to new plans; every schema is picked by its folder name.
          </p>
        </header>
        {#if schemas.active_error}
          <p class="section-error" role="alert">
            The active schema does not resolve: {schemas.active_error}
          </p>
        {/if}
        <div class="schema-list" role="list">
          {#each schemas.schemas as schema (schema.name)}
            <div class="schema-row" role="listitem" class:active={schema.name === schemas.active}>
              <span class="schema-main">
                <span class="schema-title">
                  <strong>{schema.name}</strong>
                  <span class="schema-reference">{schema.reference}</span>
                  {#if schema.name === schemas.active}<span class="current-badge">Active</span>{/if}
                </span>
                <span class="micro">{originMeta(schema).label}{#if originMeta(schema).hint} — {originMeta(schema).hint}{/if}</span>
                {#if schema.description}<span class="micro">{schema.description}</span>{/if}
                {#if schema.error}
                  <span class="schema-error">{schema.error}</span>
                {:else if schema.warnings.length > 0}
                  <span class="micro">{schema.warnings.join(" ")}</span>
                {:else}
                  <span class="micro">{schema.artifacts.join(" → ")}</span>
                {/if}
                {#if !schema.error}<span class="schema-path">{schema.path}</span>{/if}
              </span>
              {#if schema.name !== schemas.active && !schema.error}
                <button
                  class="button button-quiet schema-activate"
                  disabled={activatingName !== ""}
                  onclick={() => activate(schema.name)}
                >
                  {activatingName === schema.name ? "…" : "Make active"}
                </button>
              {/if}
            </div>
          {/each}
        </div>
        {#if activateError}<p class="section-error" role="alert">{activateError}</p>{/if}
        <div class="add-grid">
          <div class="add-row">
            <input
              class="add-input"
              bind:value={addSource}
              placeholder="schema folder or git URL"
              aria-label="Schema folder or git URL"
              spellcheck="false"
            />
            <button class="button button-primary" disabled={addBusy} onclick={addSchema}>
              {addBusy ? "Adding…" : "Add schema"}
            </button>
          </div>
          {#if addError}<p class="section-error" role="alert">{addError}</p>{/if}
          {#if addNote}<p class="section-note">{addNote}</p>{/if}
          <p class="micro">
            Added schemas land in <code>.coding/openspec/schemas/&lt;name&gt;/</code>; an invalid
            source copies nothing.
          </p>
          <div class="add-row">
            <select class="add-input" bind:value={forkFrom} aria-label="Schema to fork">
              <option value="">fork active or picked schema</option>
              {#each pickable as schema (schema.name)}
                <option value={schema.name}>{schema.name}</option>
              {/each}
            </select>
            <input
              class="add-input"
              bind:value={forkTo}
              placeholder="new name"
              aria-label="New schema name"
              spellcheck="false"
            />
            <button class="button button-quiet" disabled={forkBusy} onclick={forkSchema}>
              {forkBusy ? "Forking…" : "Fork to edit"}
            </button>
          </div>
          {#if forkError}<p class="section-error" role="alert">{forkError}</p>{/if}
        </div>
      </section>

      <section class="group" aria-label="Artifact templates">
        <header>
          <h3>Artifacts</h3>
          <p class="micro">
            Each artifact is a plan section the lead fills. Project schemas are editable here; the
            built-in and user schemas are read-only — fork first.
          </p>
        </header>
        <div class="add-row">
          <select class="add-input" bind:value={artifactSchema} onchange={() => loadArtifacts(artifactSchema)} aria-label="Schema to edit">
            {#each pickable as schema (schema.name)}
              <option value={schema.name}>{schema.name}</option>
            {/each}
          </select>
        </div>
        {#if artifactError}
          <p class="section-error" role="alert">{artifactError}</p>
        {:else if artifactsView}
          {#if artifactsView.artifacts.length === 0}
            <p class="micro">This schema has no artifacts.</p>
          {:else}
            <div class="artifact-list" role="tablist" aria-label="Artifacts">
              {#each artifactsView.artifacts as artifact (artifact.id)}
                <button
                  class="artifact-tab"
                  class:selected={artifact.id === selectedArtifactId}
                  role="tab"
                  aria-selected={artifact.id === selectedArtifactId}
                  onclick={() => {
                    selectedArtifactId = artifact.id;
                    syncDrafts();
                  }}
                >
                  {artifact.id}
                </button>
              {/each}
            </div>
            {#if selectedArtifact}
              <p class="micro">
                Generates <code>{selectedArtifact.generates}</code> from the template
                <code>{selectedArtifact.template}</code>{#if selectedArtifact.requires.length > 0},
                  after {selectedArtifact.requires.join(", ")}{/if}.
              </p>
              <div class="template-grid">
                <label class="template-pane">
                  <span class="pane-label">Template</span>
                  <textarea
                    class="template-area"
                    bind:value={templateDraft}
                    disabled={!artifactEditable}
                    aria-label={`Template for ${selectedArtifact.id}`}
                    rows="10"
                    spellcheck="false"
                  ></textarea>
                </label>
                <div class="template-pane">
                  <span class="pane-label">Preview</span>
                  <pre class="template-preview">{templatePreview}</pre>
                </div>
              </div>
              <label class="instruction-pane">
                <span class="pane-label">Instruction</span>
                <textarea
                  class="instruction-area"
                  bind:value={instructionDraft}
                  disabled={!artifactEditable}
                  aria-label={`Instruction for ${selectedArtifact.id}`}
                  rows="4"
                ></textarea>
              </label>
              <div class="section-actions">
                {#if artifactNote && !artifactDirtyNow}<p class="section-note">{artifactNote}</p>{/if}
                <div class="section-buttons">
                  <button
                    class="button button-quiet"
                    disabled={artifactBusy || !artifactDirtyNow}
                    onclick={discardArtifact}
                  >
                    Discard
                  </button>
                  {#if artifactEditable}
                    <button
                      class="button button-primary"
                      disabled={artifactBusy || !artifactDirtyNow}
                      onclick={saveArtifact}
                    >
                      {artifactBusy ? "Saving…" : "Save artifact"}
                    </button>
                  {:else}
                    <span class="micro">Read-only — fork to edit.</span>
                  {/if}
                </div>
              </div>
            {/if}
          {/if}
        {/if}
      </section>

      <section class="group" aria-label="Project context and rules">
        <header>
          <h3>Project context and rules</h3>
          <p class="micro">
            <code>.coding/openspec/config.yaml</code>: the context every worker brief carries, and
            extra rules per artifact id.
          </p>
        </header>
        <label class="context-pane">
          <span class="pane-label">Context</span>
          <textarea
            class="context-area"
            bind:value={contextDraft}
            disabled={config === null}
            aria-label="Project context"
            rows="5"
          ></textarea>
        </label>
        {#if ruleRows.length === 0}
          <p class="micro">No extra rules yet.</p>
        {:else}
          <div class="rule-list">
            {#each ruleRows as row, index (index)}
              <div class="rule-row">
                <input
                  class="rule-key"
                  bind:value={row.key}
                  placeholder="artifact id"
                  aria-label={`Rule ${index + 1} artifact id`}
                  spellcheck="false"
                />
                <textarea
                  class="rule-lines"
                  bind:value={row.text}
                  placeholder="one rule per line"
                  aria-label={`Rules for ${row.key || index + 1}`}
                  rows="2"
                ></textarea>
                <button
                  class="button button-quiet rule-remove"
                  aria-label={`Remove rule ${index + 1}`}
                  onclick={() => removeRuleRow(index)}
                >
                  Remove
                </button>
              </div>
            {/each}
          </div>
        {/if}
        <div class="rule-add">
          <button class="button button-quiet" onclick={addRuleRow}>Add rule</button>
        </div>
        {#if config?.warnings.length}
          <p class="micro">{config.warnings.join(" ")}</p>
        {/if}
        <div class="section-actions">
          {#if configError}<p class="section-error" role="alert">{configError}</p>{/if}
          {#if configSaved && !configDirty}<p class="section-note">Saved.</p>{/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={configBusy || !configDirty} onclick={discardConfig}>
              Discard
            </button>
            <button class="button button-primary" disabled={configBusy || !configDirty} onclick={saveConfig}>
              {configBusy ? "Saving…" : "Save context and rules"}
            </button>
          </div>
        </div>
      </section>

      <section class="group" aria-label="Plan preview">
        <header>
          <h3>Plan preview</h3>
          <p class="micro">
            What a new plan under one schema must contain — and the exact project-context block its
            workers' briefs would carry.
          </p>
        </header>
        <div class="add-row">
          <select class="add-input" bind:value={previewName} onchange={() => loadPreview(previewName)} aria-label="Schema to preview">
            {#each pickable as schema (schema.name)}
              <option value={schema.name}>{schema.name}</option>
            {/each}
          </select>
        </div>
        {#if previewError}
          <p class="section-error" role="alert">{previewError}</p>
        {:else if preview}
          {#if preview.required.length > 0}
            <p class="micro">
              <code>team_dispatch</code> is refused until
              {#each preview.required as id, position (id)}{position > 0 ? ", " : ""}<code>{id}</code>{/each}
              are filled.
            </p>
          {/if}
          <ul class="section-list">
            {#each preview.sections as section (section.id)}
              <li>
                <strong>{section.id}</strong>
                {#if section.required_for_dispatch}<span class="required-badge">Required</span>{/if}
                <span class="micro">— {section.generates}</span>
                {#if section.instruction}<span class="micro">{section.instruction}</span>{/if}
              </li>
            {/each}
          </ul>
          {#if preview.worker_context}
            <div class="template-pane">
              <span class="pane-label">Worker context</span>
              <pre class="template-preview">{preview.worker_context}</pre>
            </div>
          {:else}
            <p class="micro">No worker context — the config context and the instructions are empty.</p>
          {/if}
        {/if}
      </section>

      <section class="group" aria-label="Specs status">
        <header>
          <h3>Specs status</h3>
          <p class="micro">
            The current-state specs under <code>.coding/openspec/specs/</code>, the open plans whose
            spec changes have not been applied yet, and the optional
            <code>openspec validate</code> run.
          </p>
        </header>
        {#if status === null}
          <p class="micro">Status unavailable.</p>
        {:else}
          {#if status.capabilities.length === 0}
            <p class="micro">No capabilities yet — this project has no current-state specs.</p>
          {:else}
            <ul class="status-list">
              {#each status.capabilities as capability (capability.name)}
                <li class:broken={capability.format_error !== null}>{capabilityLine(capability)}</li>
              {/each}
            </ul>
          {/if}
          {#if status.pending_plans.length === 0}
            <p class="micro">No open plans carry spec changes.</p>
          {:else}
            <p class="micro">Pending spec changes (applied when the plan closes):</p>
            <ul class="status-list">
              {#each status.pending_plans as plan (plan.plan_id)}
                <li>{pendingPlanLine(plan)}</li>
              {/each}
            </ul>
          {/if}
          {#if status.openspec_cli}
            <div class="add-row">
              <button class="button button-quiet" disabled={validateBusy} onclick={runValidate}>
                {validateBusy ? "Running…" : "Run openspec validate"}
              </button>
              {#if validateOutput}<span class="micro">{validateLine(validateOutput)}</span>{/if}
            </div>
            {#if validateOutput}
              <pre class="validate-output">{validateOutput.stdout || validateOutput.stderr}</pre>
            {/if}
          {:else}
            <p class="micro">
              The <code>openspec</code> CLI is not on PATH — install it to run validation from here.
            </p>
          {/if}
        {/if}
        {#if validateError}<p class="section-error" role="alert">{validateError}</p>{/if}
      </section>
    {/if}
  </div>
</div>

<style>
  .specs-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .specs-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }
  .load-error { margin: 0; padding: 18px 24px; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  .load-note { margin: 0; padding: 18px 24px; color: var(--muted); font-size: var(--text-body); }
  .empty { padding: 18px 24px; }

  .group { display: grid; gap: 8px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--canvas); }
  .group header { display: grid; gap: 4px; }
  .group h3 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .micro { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }
  .micro code { padding: 1px 4px; border-radius: 5px; background: var(--surface-soft); font-size: 0.92em; }

  .schema-list { display: grid; gap: 4px; }
  .schema-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    font-size: var(--text-body);
  }
  .schema-row.active { border-color: var(--accent); background: var(--accent-soft); }
  .schema-main { display: grid; gap: 2px; min-width: 0; }
  .schema-title { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
  .schema-title strong { font-weight: 620; }
  .schema-reference { color: var(--muted); font-size: 0.85em; }
  .schema-path { color: var(--muted); font-size: 0.8em; overflow-wrap: anywhere; }
  .schema-error { color: var(--danger); font-size: 0.9em; overflow-wrap: anywhere; }
  .current-badge { color: var(--muted); font-size: 0.82em; }
  .schema-activate { min-height: 32px; padding: 5px 10px; flex-shrink: 0; }

  .add-grid { display: grid; gap: 6px; }
  .add-row { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .add-input {
    flex: 1;
    min-width: 180px;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface);
    font-size: var(--text-body);
  }

  .artifact-list { display: flex; gap: 6px; flex-wrap: wrap; }
  .artifact-tab {
    padding: 6px 12px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    cursor: pointer;
    font-size: var(--text-body);
  }
  .artifact-tab.selected { border-color: var(--accent); background: var(--accent-soft); font-weight: 620; }

  .template-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .template-pane { display: grid; gap: 4px; min-width: 0; }
  .pane-label { color: var(--muted); font-size: 0.82em; font-weight: 620; letter-spacing: 0.02em; text-transform: uppercase; }
  .template-area, .instruction-area, .context-area {
    width: 100%;
    padding: 9px 11px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface);
    color: inherit;
    font-family: var(--font-mono, monospace);
    font-size: 0.92em;
    line-height: 1.5;
    resize: vertical;
  }
  .instruction-pane, .context-pane { display: grid; gap: 4px; }
  .template-preview, .validate-output {
    margin: 0;
    padding: 9px 11px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface-soft);
    overflow: auto;
    font-family: var(--font-mono, monospace);
    font-size: 0.88em;
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .rule-list { display: grid; gap: 6px; }
  .rule-row { display: grid; grid-template-columns: minmax(120px, 200px) 1fr auto; gap: 8px; align-items: start; }
  .rule-key {
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface);
    font-size: var(--text-body);
  }
  .rule-lines {
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface);
    font-family: var(--font-mono, monospace);
    font-size: 0.9em;
    line-height: 1.5;
    resize: vertical;
  }
  .rule-remove { min-height: 32px; padding: 5px 10px; }
  .rule-add { display: flex; justify-content: flex-start; }
  .rule-add .button { min-height: 32px; padding: 5px 10px; }

  .section-list { display: grid; gap: 6px; margin: 0; padding-left: 20px; font-size: var(--text-body); }
  .section-list li { display: grid; gap: 2px; }
  .required-badge {
    margin-left: 6px;
    padding: 1px 7px;
    border: 1px solid var(--accent);
    border-radius: 999px;
    color: var(--accent);
    font-size: 0.78em;
    font-weight: 700;
  }

  .status-list { display: grid; gap: 4px; margin: 0; padding-left: 20px; font-size: var(--text-body); }
  .status-list li.broken { color: var(--danger); }

  .section-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 2px; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; flex: 1; }
  .section-note { margin: 0; color: var(--muted); font-size: var(--text-body); flex: 1; }
  .section-buttons { display: flex; gap: 8px; align-items: center; flex-shrink: 0; }

  @media (max-width: 720px) {
    .template-grid { grid-template-columns: 1fr; }
    .rule-row { grid-template-columns: 1fr; }
  }
</style>
