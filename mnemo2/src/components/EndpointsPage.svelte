<script lang="ts">
  import { ChevronDown, ChevronRight, Plus } from "@lucide/svelte";
  import {
    emptyEndpoint,
    emptyModel,
    type EndpointDetailDto,
    type EndpointTestDto,
    type ModelDetailDto,
    type ProviderModelDto,
    type SettingsTransport,
    type TiersDto,
  } from "../lib/settingsEngine";
  import { ENDPOINT_KINDS, ENDPOINT_PRESETS, errorText, tiersListing, type EndpointPreset } from "../lib/settingsState";

  let {
    transport,
    endpoints,
    tiers,
    onendpoints,
  }: {
    transport: SettingsTransport;
    endpoints: EndpointDetailDto[];
    tiers: TiersDto;
    onendpoints: (next: EndpointDetailDto[]) => void;
  } = $props();

  let draft = $state<EndpointDetailDto | null>(null);
  let snapshot = $state("");
  let originalName = $state<string | null>(null);
  let savedIds = $state<string[]>([]);
  let apiKey = $state("");
  let picking = $state(false);
  let pendingAction = $state<(() => void) | null>(null);
  let expanded = $state<number | null>(null);
  let newModelId = $state("");
  let testResult = $state<EndpointTestDto | null>(null);
  let testing = $state(false);
  let fetched = $state<ProviderModelDto[] | null>(null);
  let fetchError = $state("");
  let fetching = $state(false);
  let ticked = $state<string[]>([]);
  let saveError = $state("");
  let saving = $state(false);
  let deleteState = $state<{ usage: string[] } | null>(null);
  let deleteError = $state("");

  const dirty = $derived(draft !== null && (JSON.stringify(draft) !== snapshot || apiKey !== ""));
  const isNew = $derived(originalName === null);

  function clone<T>(value: T): T {
    return JSON.parse(JSON.stringify(value)) as T;
  }

  function resetTransient() {
    apiKey = "";
    expanded = null;
    newModelId = "";
    testResult = null;
    fetched = null;
    fetchError = "";
    ticked = [];
    saveError = "";
    deleteState = null;
    deleteError = "";
    pendingAction = null;
  }

  function load(endpoint: EndpointDetailDto, name: string | null) {
    resetTransient();
    draft = clone(endpoint);
    snapshot = JSON.stringify(draft);
    originalName = name;
    savedIds = name === null ? [] : endpoint.models.map((model) => model.id);
    picking = false;
  }

  function select(name: string) {
    const endpoint = endpoints.find((entry) => entry.name === name);
    if (endpoint) load(endpoint, endpoint.name);
  }

  /** Run `action` now, or after the user agrees to drop unsaved edits. */
  function guard(action: () => void) {
    if (dirty) pendingAction = action;
    else action();
  }

  function startAdd() {
    guard(() => {
      resetTransient();
      draft = null;
      originalName = null;
      picking = true;
    });
  }

  function usePreset(preset: EndpointPreset | null) {
    const base = emptyEndpoint();
    if (preset) {
      base.kind = preset.kind;
      base.base_url = preset.baseUrl;
      base.name = uniqueName(preset.name);
    }
    load(base, null);
    snapshot = "";
  }

  function uniqueName(base: string): string {
    const names = new Set(endpoints.map((entry) => entry.name));
    if (!names.has(base)) return base;
    for (let n = 2; ; n += 1) if (!names.has(`${base}-${n}`)) return `${base}-${n}`;
  }

  $effect(() => {
    if (draft === null && !picking && endpoints.length > 0) load(endpoints[0], endpoints[0].name);
  });

  function discard() {
    if (!draft) return;
    const current = originalName === null ? null : endpoints.find((entry) => entry.name === originalName);
    if (current) load(current, current.name);
    else {
      resetTransient();
      draft = null;
    }
  }

  function probe() {
    return { baseUrl: draft?.base_url ?? null, kind: draft?.kind ?? null, apiKey: apiKey || null };
  }

  async function test() {
    if (!draft) return;
    testing = true;
    testResult = null;
    try {
      testResult = await transport.testEndpoint(originalName ?? draft.name, probe());
    } catch (failure) {
      testResult = { ok: false, model_count: 0, error: errorText(failure) };
    } finally {
      testing = false;
    }
  }

  async function fetchModels() {
    if (!draft) return;
    fetching = true;
    fetchError = "";
    try {
      const have = new Set(draft.models.map((model) => model.id));
      fetched = (await transport.listProviderModels(originalName ?? draft.name, probe())).filter((entry) => !have.has(entry.id));
      ticked = [];
    } catch (failure) {
      fetched = null;
      fetchError = errorText(failure);
    } finally {
      fetching = false;
    }
  }

  function addModel(model: ModelDetailDto) {
    if (!draft || !model.id || draft.models.some((entry) => entry.id === model.id)) return;
    draft.models = [...draft.models, model];
  }

  function addTyped() {
    const id = newModelId.trim();
    if (id) addModel(emptyModel(id));
    newModelId = "";
  }

  function addTicked() {
    for (const entry of fetched ?? []) {
      if (!ticked.includes(entry.id)) continue;
      const model = emptyModel(entry.id);
      model.max_context = entry.context_length;
      model.max_output_tokens = entry.max_output_tokens;
      if (entry.vision_capable) model.multimodal = true;
      addModel(model);
    }
    fetched = (fetched ?? []).filter((entry) => !ticked.includes(entry.id));
    ticked = [];
  }

  function removeModel(index: number) {
    if (!draft) return;
    draft.models = draft.models.filter((_, i) => i !== index);
    expanded = null;
  }

  function renameModel(index: number, event: Event) {
    if (!draft) return;
    const input = event.currentTarget as HTMLInputElement;
    const next = input.value.trim();
    const model = draft.models[index];
    if (!next || draft.models.some((entry, i) => i !== index && entry.id === next)) {
      input.value = model.id;
      return;
    }
    if (savedIds.includes(model.id) && !model.previous_id) model.previous_id = model.id;
    model.id = next;
    if (model.previous_id === next) model.previous_id = null;
  }

  function price(model: ModelDetailDto, field: "input_per_1m" | "output_per_1m" | "cached_per_1m", event: Event) {
    const raw = (event.currentTarget as HTMLInputElement).value;
    const value = raw === "" ? 0 : Number(raw);
    if (!Number.isFinite(value)) return;
    const next = { input_per_1m: 0, output_per_1m: 0, cached_per_1m: 0, ...(model.price ?? {}), [field]: value };
    model.price = next.input_per_1m === 0 && next.output_per_1m === 0 && next.cached_per_1m === 0 ? null : next;
  }

  function numberField(event: Event): number | null {
    const raw = (event.currentTarget as HTMLInputElement).value;
    if (raw === "") return null;
    const value = Number(raw);
    return Number.isFinite(value) ? value : null;
  }

  function triState(value: boolean | null): string {
    return value === null ? "auto" : value ? "on" : "off";
  }

  function fromTriState(value: string): boolean | null {
    return value === "on" ? true : value === "off" ? false : null;
  }

  function setEfforts(model: ModelDetailDto, event: Event) {
    model.reasoning_efforts = (event.currentTarget as HTMLInputElement).value
      .split(",")
      .map((entry) => entry.trim())
      .filter(Boolean);
  }

  async function save() {
    if (!draft) return;
    const name = draft.name.trim();
    if (!name) {
      saveError = "Give the endpoint a name.";
      return;
    }
    if (!draft.base_url.trim()) {
      saveError = "Enter a base URL.";
      return;
    }
    saving = true;
    saveError = "";
    try {
      const endpoint = { ...clone(draft), name, base_url: draft.base_url.trim() };
      const previous = originalName !== null && originalName !== name ? originalName : null;
      let list = await transport.upsertEndpoint(endpoint, previous);
      if (apiKey) {
        await transport.setApiKey(name, apiKey);
        list = await transport.endpoints();
      }
      onendpoints(list);
      const saved = list.find((entry) => entry.name === name);
      if (saved) load(saved, saved.name);
    } catch (failure) {
      saveError = errorText(failure);
    } finally {
      saving = false;
    }
  }

  async function askDelete() {
    if (originalName === null) return;
    deleteError = "";
    try {
      deleteState = { usage: await transport.endpointUsage(originalName) };
    } catch (failure) {
      deleteError = errorText(failure);
    }
  }

  async function confirmDelete() {
    if (originalName === null) return;
    try {
      const list = await transport.removeEndpoint(originalName);
      onendpoints(list);
      resetTransient();
      draft = null;
      picking = false;
      if (list.length > 0) load(list[0], list[0].name);
    } catch (failure) {
      deleteError = errorText(failure);
    }
  }

  function dotOn(endpoint: EndpointDetailDto): boolean {
    return endpoint.has_api_key || endpoint.kind === "local";
  }
</script>

<div class="endpoints-page">
  <div class="endpoints-body">
    <aside class="endpoint-list" aria-label="Endpoints">
      <ul>
        {#each endpoints as endpoint (endpoint.name)}
          <li>
            <button class:active={draft !== null && originalName === endpoint.name} aria-label={`Edit ${endpoint.name}`} onclick={() => guard(() => select(endpoint.name))}>
              <span class="dot" class:on={dotOn(endpoint)} title={dotOn(endpoint) ? "Ready" : "No API key"}></span>
              <span class="ep-name">{endpoint.name}</span>
              <span class="ep-meta">{endpoint.kind} · {endpoint.models.length} {endpoint.models.length === 1 ? "model" : "models"}</span>
            </button>
          </li>
        {/each}
      </ul>
      <button class="button button-quiet add-endpoint" aria-label="Add endpoint" onclick={startAdd}><Plus size={12} /> Add endpoint</button>
    </aside>

    <div class="editor">
      {#if pendingAction}
        <div class="inline-confirm" role="alert">
          <span>Discard unsaved changes?</span>
          <button class="button button-quiet" onclick={() => (pendingAction = null)}>Keep editing</button>
          <button class="button button-primary" onclick={() => { const run = pendingAction; pendingAction = null; if (draft) { snapshot = JSON.stringify(draft); apiKey = ""; } run?.(); }}>Discard changes</button>
        </div>
      {/if}

      {#if picking}
        <div class="pad">
          <p class="micro">NEW ENDPOINT</p>
          <h3>Choose a provider</h3>
          <div class="presets">
            {#each ENDPOINT_PRESETS as preset (preset.id)}
              <button class="preset" onclick={() => usePreset(preset)}>
                <strong>{preset.label}</strong>
                <span>{preset.baseUrl}</span>
              </button>
            {/each}
            <button class="preset" onclick={() => usePreset(null)}>
              <strong>Custom</strong>
              <span>Start from a blank endpoint</span>
            </button>
          </div>
        </div>
      {:else if draft}
        <div class="editor-scroll">
          <section class="group">
            <p class="micro">CONNECTION</p>
            <div class="grid-conn">
              <label class="field"><span>Name</span><input aria-label="Endpoint name" bind:value={draft.name} autocomplete="off" /></label>
              <label class="field"><span>Kind</span>
                <select aria-label="Endpoint kind" bind:value={draft.kind}>
                  {#each ENDPOINT_KINDS as kind (kind.value)}<option value={kind.value}>{kind.label}</option>{/each}
                </select>
              </label>
              <label class="field wide"><span>Base URL</span><input aria-label="Base URL" bind:value={draft.base_url} autocomplete="off" spellcheck="false" /></label>
              <label class="field wide"><span>API key</span>
                <input aria-label="API key" type="password" autocomplete="off" bind:value={apiKey} placeholder={draft.has_api_key ? "saved" : ""} />
              </label>
            </div>
            <div class="test-row">
              <button class="button button-quiet" disabled={testing} onclick={test}>{testing ? "Testing…" : "Test connection"}</button>
              {#if testResult}
                {#if testResult.ok}
                  <span class="result ok" role="status">Connected · {testResult.model_count} {testResult.model_count === 1 ? "model" : "models"}</span>
                {:else}
                  <span class="result bad" role="status">{testResult.error ?? "Connection failed"}</span>
                {/if}
              {/if}
            </div>
          </section>

          <section class="group">
            <p class="micro">MODELS</p>
            {#if draft.models.length === 0}
              <p class="empty-note">No models yet. Add an id below or fetch the provider's list.</p>
            {:else}
              <div class="models" role="table" aria-label="Models">
                <div class="m-row m-head" role="row">
                  <span role="columnheader"></span><span role="columnheader">Model id</span><span role="columnheader">Tier</span>
                  <span role="columnheader" class="num">$ in</span><span role="columnheader" class="num">$ out</span><span role="columnheader" class="num">Context</span>
                </div>
                {#each draft.models as model, index (index)}
                  <div class="m-row" role="row">
                    <button class="icon-button tiny" aria-label={`Details for ${model.id}`} aria-expanded={expanded === index} onclick={() => (expanded = expanded === index ? null : index)}>
                      {#if expanded === index}<ChevronDown size={12} />{:else}<ChevronRight size={12} />{/if}
                    </button>
                    <input class="mono" aria-label={`Model id ${model.id}`} value={model.id} onchange={(event) => renameModel(index, event)} spellcheck="false" />
                    <span class="tier-label">{tiersListing(tiers, `${originalName ?? draft.name}/${model.id}`).join(", ") || "—"}</span>
                    <input class="num" type="number" min="0" step="any" aria-label={`Input price for ${model.id}`} value={model.price?.input_per_1m ?? ""} onchange={(event) => price(model, "input_per_1m", event)} />
                    <input class="num" type="number" min="0" step="any" aria-label={`Output price for ${model.id}`} value={model.price?.output_per_1m ?? ""} onchange={(event) => price(model, "output_per_1m", event)} />
                    <input class="num" type="number" min="0" step="1" aria-label={`Context for ${model.id}`} bind:value={model.max_context} />
                  </div>
                  {#if expanded === index}
                    <div class="m-advanced">
                      <p class="micro">ADVANCED · {model.id}</p>
                      <div class="grid-adv">
                        <label class="field"><span>Max output tokens</span><input type="number" min="0" step="1" aria-label={`Max output tokens for ${model.id}`} bind:value={model.max_output_tokens} /></label>
                        <label class="field"><span>Temperature</span><input type="number" min="0" step="any" aria-label={`Temperature for ${model.id}`} bind:value={model.temperature} /></label>
                        <label class="field"><span>Cached price / 1M</span><input type="number" min="0" step="any" aria-label={`Cached price for ${model.id}`} value={model.price?.cached_per_1m ?? ""} onchange={(event) => price(model, "cached_per_1m", event)} /></label>
                        <label class="field"><span>Vision</span>
                          <select aria-label={`Vision for ${model.id}`} value={model.multimodal === null ? "inherit" : model.multimodal ? "on" : "off"} onchange={(event) => { const v = (event.currentTarget as HTMLSelectElement).value; model.multimodal = v === "inherit" ? null : v === "on"; }}>
                            <option value="inherit">Inherit</option><option value="on">On</option><option value="off">Off</option>
                          </select>
                        </label>
                        <label class="field"><span>Reasoning efforts</span><input aria-label={`Reasoning efforts for ${model.id}`} placeholder="high, medium, low" value={model.reasoning_efforts.join(", ")} onchange={(event) => setEfforts(model, event)} /></label>
                        <label class="field"><span>Default effort</span>
                          <input aria-label={`Default effort for ${model.id}`} value={model.reasoning_effort ?? ""} onchange={(event) => { model.reasoning_effort = (event.currentTarget as HTMLInputElement).value.trim() || null; }} />
                        </label>
                      </div>
                      <button class="button button-quiet danger-text" aria-label={`Remove model ${model.id}`} onclick={() => removeModel(index)}>Remove model</button>
                    </div>
                  {/if}
                {/each}
              </div>
            {/if}

            <div class="add-model">
              <input aria-label="New model id" placeholder="model-id" spellcheck="false" bind:value={newModelId} onkeydown={(event) => { if (event.key === "Enter") { event.preventDefault(); addTyped(); } }} />
              <button class="button button-quiet" aria-label="Add model" disabled={!newModelId.trim()} onclick={addTyped}>Add model</button>
              <button class="button button-quiet" disabled={fetching} onclick={fetchModels}>{fetching ? "Fetching…" : "Fetch from provider"}</button>
            </div>
            {#if fetchError}<p class="result bad" role="alert">{fetchError}</p>{/if}
            {#if fetched}
              {#if fetched.length === 0}
                <p class="empty-note">Nothing new from the provider.</p>
              {:else}
                <div class="fetched">
                  <ul>
                    {#each fetched as entry (entry.id)}
                      <li>
                        <label>
                          <input type="checkbox" checked={ticked.includes(entry.id)} onchange={(event) => { ticked = (event.currentTarget as HTMLInputElement).checked ? [...ticked, entry.id] : ticked.filter((id) => id !== entry.id); }} />
                          <span class="mono">{entry.id}</span>
                        </label>
                        <span class="hint">{entry.vision_capable ? "vision · " : ""}{entry.context_length ? `${entry.context_length.toLocaleString("en-US")} ctx` : ""}</span>
                      </li>
                    {/each}
                  </ul>
                  <button class="button button-primary" disabled={ticked.length === 0} onclick={addTicked}>Add {ticked.length || ""} selected</button>
                </div>
              {/if}
            {/if}
          </section>

          <details class="group">
            <summary><span class="micro">ADVANCED</span></summary>
            <div class="grid-adv">
              <label class="field"><span>Max context</span><input type="number" min="0" step="1" aria-label="Endpoint max context" bind:value={draft.max_context} /></label>
              <label class="field"><span>Max output tokens</span><input type="number" min="0" step="1" aria-label="Endpoint max output tokens" bind:value={draft.max_output_tokens} /></label>
              <label class="field"><span>Temperature</span><input type="number" min="0" step="any" aria-label="Endpoint temperature" bind:value={draft.temperature} /></label>
              <label class="field"><span>Vision default</span>
                <select aria-label="Endpoint vision default" value={draft.multimodal ? "on" : "off"} onchange={(event) => (draft!.multimodal = (event.currentTarget as HTMLSelectElement).value === "on")}>
                  <option value="off">Off</option><option value="on">On</option>
                </select>
              </label>
              <label class="field"><span>Reasoning effort</span>
                <select aria-label="Endpoint reasoning effort support" value={draft.supports_reasoning_effort ? "on" : "off"} onchange={(event) => (draft!.supports_reasoning_effort = (event.currentTarget as HTMLSelectElement).value === "on")}>
                  <option value="on">Supported</option><option value="off">Not supported</option>
                </select>
              </label>
              <label class="field"><span>Default effort</span><input aria-label="Endpoint default effort" value={draft.reasoning_effort ?? ""} onchange={(event) => (draft!.reasoning_effort = (event.currentTarget as HTMLInputElement).value.trim() || null)} /></label>
              <label class="field"><span>Strict schemas</span>
                <select aria-label="Strict schemas" value={triState(draft.supports_strict_schema)} onchange={(event) => (draft!.supports_strict_schema = fromTriState((event.currentTarget as HTMLSelectElement).value))}>
                  <option value="auto">Auto</option><option value="on">On</option><option value="off">Off</option>
                </select>
              </label>
              {#if draft.kind === "anthropic"}
                <label class="field"><span>Workspace id</span><input aria-label="Workspace id" value={draft.workspace_id ?? ""} onchange={(event) => (draft!.workspace_id = (event.currentTarget as HTMLInputElement).value.trim() || null)} /></label>
              {/if}
            </div>
          </details>

          {#if !isNew}
            <section class="group danger-zone">
              {#if deleteState}
                <div class="delete-confirm" role="alert">
                  <strong>Delete {originalName}?</strong>
                  {#if deleteState.usage.length > 0}
                    <p>Used by:</p>
                    <ul>{#each deleteState.usage as use (use)}<li>{use}</li>{/each}</ul>
                  {:else}
                    <p>Nothing references this endpoint.</p>
                  {/if}
                  <div class="row">
                    <button class="button button-quiet" onclick={() => (deleteState = null)}>Cancel</button>
                    <button class="button button-danger" onclick={confirmDelete}>Confirm delete</button>
                  </div>
                </div>
              {:else}
                <button class="button button-quiet danger-text" aria-label="Delete endpoint" onclick={askDelete}>Delete endpoint</button>
              {/if}
              {#if deleteError}<p class="result bad" role="alert">{deleteError}</p>{/if}
            </section>
          {/if}
        </div>

        {#if dirty || saveError}
          <footer class="settings-footer">
            {#if saveError}<p class="footer-error" role="alert">{saveError}</p>{:else}<span class="footer-note">Unsaved changes</span>{/if}
            <div>
              <button class="button button-quiet" disabled={saving} onclick={discard}>Discard</button>
              <button class="button button-primary" disabled={saving || !dirty} onclick={save}>{saving ? "Saving…" : "Save"}</button>
            </div>
          </footer>
        {/if}
      {:else}
        <div class="pad empty">
          <p>No endpoints yet — add one to connect a provider.</p>
          <button class="button button-primary" onclick={startAdd}><Plus size={12} /> Add endpoint</button>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .endpoints-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .endpoints-body { display: grid; min-height: 0; flex: 1; grid-template-columns: 210px minmax(0, 1fr); }
  .endpoint-list { display: flex; min-height: 0; flex-direction: column; gap: 8px; padding: 12px 10px; border-right: 1px solid var(--line); }
  .endpoint-list ul { display: grid; flex: 1; align-content: start; gap: 2px; margin: 0; padding: 0; overflow-y: auto; list-style: none; }
  .endpoint-list li button { display: grid; width: 100%; grid-template-columns: 8px 1fr; align-items: center; column-gap: 8px; padding: 7px 8px; border: 0; border-radius: 8px; background: transparent; cursor: pointer; text-align: left; }
  .endpoint-list li button:hover { background: var(--surface-soft); }
  .endpoint-list li button.active { background: var(--accent-soft); }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--line-strong); }
  .dot.on { background: var(--accent); }
  .ep-name { overflow: hidden; font-size: var(--text-body); font-weight: 650; text-overflow: ellipsis; white-space: nowrap; }
  .ep-meta { grid-column: 2; color: var(--muted); font-size: var(--text-meta); }
  .add-endpoint { min-height: 30px; }
  .editor { display: flex; min-width: 0; min-height: 0; flex-direction: column; }
  .editor-scroll { display: grid; flex: 1; align-content: start; gap: 20px; padding: 16px 22px; overflow-y: auto; }
  .pad { padding: 20px 22px; }
  .pad.empty { display: grid; justify-items: start; gap: 12px; color: var(--muted); font-size: var(--text-body); }
  .pad.empty p { margin: 0; }
  .pad h3 { margin: 4px 0 12px; font-family: var(--font-prose); font-size: var(--text-title); font-weight: 550; }
  .presets { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; }
  .preset { display: grid; gap: 3px; padding: 10px 12px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); cursor: pointer; text-align: left; }
  .preset:hover { border-color: var(--line-strong); background: var(--surface-soft); }
  .preset strong { font-size: var(--text-body); }
  .preset span { overflow: hidden; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .micro { margin: 0 0 8px; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.11em; }
  .group { display: block; }
  summary { cursor: pointer; }
  summary .micro { display: inline; }
  details > .grid-adv { margin-top: 10px; }
  .grid-conn { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .grid-adv { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; }
  .field { display: grid; gap: 4px; }
  .field.wide { grid-column: 1 / -1; }
  .field > span { color: var(--muted); font-size: var(--text-meta); }
  input, select { min-width: 0; padding: 6px 8px; border: 1px solid var(--line); border-radius: 7px; background: var(--surface); color: var(--ink); font-size: var(--text-body); }
  input:hover, select:hover { border-color: var(--line-strong); }
  input[type="checkbox"] { padding: 0; }
  .mono { font-family: var(--font-mono); }
  .num { font-variant-numeric: tabular-nums; text-align: right; }
  .test-row { display: flex; align-items: center; gap: 10px; margin-top: 10px; }
  .result { font-size: var(--text-body); overflow-wrap: anywhere; }
  .result.ok { color: var(--accent-strong); }
  .result.bad { margin: 6px 0 0; color: var(--danger); }
  .empty-note { margin: 0 0 8px; color: var(--muted); font-size: var(--text-body); }
  .models { border: 1px solid var(--line); border-radius: 12px; overflow: hidden; }
  .m-row { display: grid; align-items: center; grid-template-columns: 26px minmax(0, 1.6fr) minmax(0, 0.8fr) 66px 66px 84px; gap: 6px; padding: 5px 8px; }
  .m-row + .m-row, .m-row + .m-advanced { border-top: 1px solid var(--line); }
  .m-head { background: var(--surface-soft); color: var(--muted); font-size: var(--text-meta); }
  .m-head .num { text-align: right; }
  .tier-label { overflow: hidden; color: var(--muted); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .icon-button.tiny { width: 22px; height: 22px; border-radius: 6px; }
  .m-advanced { display: grid; gap: 8px; padding: 10px 12px 12px 34px; border-top: 1px solid var(--line); background: var(--canvas); }
  .m-advanced .micro { margin: 0; }
  .add-model { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 10px; }
  .add-model input { flex: 1; min-width: 140px; font-family: var(--font-mono); }
  .fetched { display: grid; gap: 8px; margin-top: 8px; padding: 8px; border: 1px solid var(--line); border-radius: 12px; }
  .fetched ul { display: grid; max-height: 160px; margin: 0; padding: 0; overflow-y: auto; list-style: none; }
  .fetched li { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 3px 4px; font-size: var(--text-body); }
  .fetched label { display: flex; align-items: center; gap: 7px; }
  .hint { color: var(--muted); font-size: var(--text-meta); font-variant-numeric: tabular-nums; }
  .danger-zone { padding-top: 14px; border-top: 1px solid var(--line); }
  .danger-text { color: var(--danger); }
  .delete-confirm { display: grid; gap: 6px; padding: 10px 12px; border: 1px solid #e3c7c2; border-radius: 12px; background: #fbf3f1; font-size: var(--text-body); }
  .delete-confirm p { margin: 0; color: var(--muted); }
  .delete-confirm ul { margin: 0; padding-left: 16px; color: var(--ink-soft); }
  .row { display: flex; justify-content: flex-end; gap: 8px; }
  .button-danger { background: var(--danger); color: white; }
  .inline-confirm { display: flex; align-items: center; justify-content: flex-end; gap: 8px; padding: 8px 22px; border-bottom: 1px solid var(--line); background: var(--warning-soft); color: var(--warning); font-size: var(--text-body); }
  .inline-confirm span { margin-right: auto; font-weight: 600; }
  .settings-footer { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 22px; border-top: 1px solid var(--line); background: var(--surface); }
  .settings-footer div { display: flex; gap: 8px; }
  .footer-note { color: var(--muted); font-size: var(--text-body); }
  .footer-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  @media (max-width: 700px) { .endpoints-body { grid-template-columns: 1fr; } .endpoint-list { border-right: 0; border-bottom: 1px solid var(--line); max-height: 160px; } .grid-adv { grid-template-columns: 1fr 1fr; } }
</style>
