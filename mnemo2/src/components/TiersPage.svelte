<script lang="ts">
  import { ArrowDown, ArrowUp, X } from "@lucide/svelte";
  import { TIER_NAMES, type EndpointDetailDto, type SettingsTransport, type TierName, type TiersDto } from "../lib/settingsEngine";
  import { allSpecs, cloneTiers, errorText, formatPrice, moveItem, priceOfSpec, TIER_META } from "../lib/settingsState";

  let {
    transport,
    endpoints,
    tiers,
    ontiers,
  }: {
    transport: SettingsTransport;
    endpoints: EndpointDetailDto[];
    tiers: TiersDto;
    ontiers: (next: TiersDto) => void;
  } = $props();

  // The page is remounted per visit, so seeding from the prop once is intended.
  // svelte-ignore state_referenced_locally
  let draft = $state<TiersDto>(cloneTiers(tiers));
  // svelte-ignore state_referenced_locally
  let snapshot = $state(JSON.stringify(tiers));
  let busy = $state(false);
  let error = $state("");

  const dirty = $derived(JSON.stringify(draft) !== snapshot);
  const specs = $derived(allSpecs(endpoints));

  function candidates(name: TierName) {
    return specs.filter((entry) => !draft[name].includes(entry.spec));
  }

  function add(name: TierName, event: Event) {
    const select = event.currentTarget as HTMLSelectElement;
    const spec = select.value;
    select.value = "";
    if (spec) draft[name] = [...draft[name], spec];
  }

  function move(name: TierName, index: number, delta: number) {
    draft[name] = moveItem(draft[name], index, delta);
  }

  function remove(name: TierName, index: number) {
    draft[name] = draft[name].filter((_, i) => i !== index);
  }

  function discard() {
    draft = cloneTiers(tiers);
    snapshot = JSON.stringify(tiers);
    error = "";
  }

  async function save() {
    busy = true;
    error = "";
    try {
      const stored = await transport.saveTiers($state.snapshot(draft) as TiersDto);
      draft = cloneTiers(stored);
      snapshot = JSON.stringify(stored);
      ontiers(stored);
    } catch (failure) {
      error = errorText(failure);
    } finally {
      busy = false;
    }
  }
</script>

<div class="tiers-page">
  <div class="tiers-scroll">
    <p class="tiers-help">The lead picks a tier for each worker; the first model in a tier is used unless the lead names another.</p>
    {#each TIER_NAMES as name (name)}
      {@const meta = TIER_META[name]}
      {@const options = candidates(name)}
      <section class="tier" aria-label={`${meta.label} tier`}>
        <header>
          <div><h3>{meta.label}</h3><p>{meta.blurb}</p></div>
          <select aria-label={`Add model to ${meta.label}`} onchange={(event) => add(name, event)} disabled={options.length === 0}>
            <option value="">{options.length === 0 ? "No more models" : "Add model…"}</option>
            {#each options as entry (entry.spec)}
              <option value={entry.spec}>{entry.spec} — {formatPrice(entry.input, entry.output)}</option>
            {/each}
          </select>
        </header>
        {#if draft[name].length === 0}
          <p class="tier-empty">No models. Add one to make this tier usable.</p>
        {:else}
          <ol>
            {#each draft[name] as spec, index (spec)}
              <li>
                <span class="spec">{spec}</span>
                <span class="default">{#if index === 0}<em>Default</em>{/if}</span>
                <span class="price">{priceOfSpec(endpoints, spec)}</span>
                <span class="row-actions">
                  <button class="icon-button" aria-label={`Move ${spec} up in ${meta.label}`} disabled={index === 0} onclick={() => move(name, index, -1)}><ArrowUp size={12} /></button>
                  <button class="icon-button" aria-label={`Move ${spec} down in ${meta.label}`} disabled={index === draft[name].length - 1} onclick={() => move(name, index, 1)}><ArrowDown size={12} /></button>
                  <button class="icon-button" aria-label={`Remove ${spec} from ${meta.label}`} onclick={() => remove(name, index)}><X size={12} /></button>
                </span>
              </li>
            {/each}
          </ol>
        {/if}
      </section>
    {/each}
  </div>
  {#if dirty || error}
    <footer class="settings-footer">
      {#if error}<p class="footer-error" role="alert">{error}</p>{:else}<span class="footer-note">Unsaved changes</span>{/if}
      <div>
        <button class="button button-quiet" disabled={busy} onclick={discard}>Discard</button>
        <button class="button button-primary" disabled={busy || !dirty} onclick={save}>{busy ? "Saving…" : "Save tiers"}</button>
      </div>
    </footer>
  {/if}
</div>

<style>
  .tiers-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .tiers-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }
  .tiers-help { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }
  .tier header { display: flex; align-items: flex-end; justify-content: space-between; gap: 12px; margin-bottom: 6px; }
  .tier h3 { margin: 0; font-size: var(--text-body); font-weight: 650; }
  .tier header p { margin: 2px 0 0; color: var(--muted); font-size: var(--text-meta); }
  .tier select { width: 260px; max-width: 100%; padding: 5px 7px; border: 1px solid var(--line); border-radius: 7px; background: var(--surface); color: var(--ink); font-size: var(--text-body); }
  .tier ol { margin: 0; padding: 0; overflow: hidden; border: 1px solid var(--line); border-radius: 12px; list-style: none; }
  .tier li { display: grid; align-items: center; grid-template-columns: minmax(0, 1fr) 54px 110px auto; gap: 8px; padding: 6px 8px 6px 12px; }
  .tier li + li { border-top: 1px solid var(--line); }
  .spec { overflow: hidden; font-family: var(--font-mono); font-size: var(--text-body); text-overflow: ellipsis; white-space: nowrap; }
  .default em { padding: 2px 6px; border-radius: 6px; background: var(--accent-soft); color: var(--accent-strong); font-size: var(--text-meta); font-style: normal; font-weight: 650; letter-spacing: 0.04em; text-transform: uppercase; }
  .price { color: var(--ink-soft); font-size: var(--text-body); font-variant-numeric: tabular-nums; text-align: right; }
  .row-actions { display: flex; gap: 3px; }
  .row-actions .icon-button { width: 24px; height: 24px; border-radius: 7px; }
  .row-actions .icon-button:disabled { opacity: 0.35; cursor: default; }
  .tier-empty { margin: 0; padding: 12px; border: 1px dashed var(--line-strong); border-radius: 12px; color: var(--muted); font-size: var(--text-body); }
  .settings-footer { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 24px; border-top: 1px solid var(--line); background: var(--surface); }
  .settings-footer div { display: flex; gap: 8px; }
  .footer-note { color: var(--muted); font-size: var(--text-body); }
  .footer-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
</style>
