<script lang="ts">
  import { createSettingsTransport, type EndpointDetailDto, type TiersDto } from "../lib/settingsEngine";
  import { errorText } from "../lib/settingsState";
  import EndpointsPage from "./EndpointsPage.svelte";
  import Sheet from "./Sheet.svelte";
  import TiersPage from "./TiersPage.svelte";

  let { open, onclose }: { open: boolean; onclose: () => void } = $props();

  type Page = "endpoints" | "tiers";
  const pages: { id: Page; label: string }[] = [
    { id: "endpoints", label: "Endpoints" },
    { id: "tiers", label: "Tiers" },
  ];

  let page = $state<Page>("endpoints");
  let endpoints = $state<EndpointDetailDto[]>([]);
  let tiers = $state<TiersDto>({ difficult: [], average: [], simple: [] });
  let loading = $state(false);
  let loadError = $state("");
  const transport = $derived(open ? createSettingsTransport() : null);

  $effect(() => {
    const active = transport;
    if (!active) return;
    let cancelled = false;
    loading = true;
    loadError = "";
    Promise.all([active.endpoints(), active.tiers()])
      .then(([loadedEndpoints, loadedTiers]) => {
        if (cancelled) return;
        endpoints = loadedEndpoints;
        tiers = loadedTiers;
      })
      .catch((error) => {
        if (!cancelled) loadError = errorText(error);
      })
      .finally(() => {
        if (!cancelled) loading = false;
      });
    return () => {
      cancelled = true;
    };
  });

  /** Renames and removals rewrite tier specs on the backend, so re-read them. */
  function refreshAfterEndpointChange(next: EndpointDetailDto[]) {
    endpoints = next;
    void transport?.tiers().then((loaded) => (tiers = loaded)).catch(() => {});
  }
</script>

<Sheet {open} title="Settings" eyebrow="MNEMO" wide {onclose}>
  {#if !transport}
    <p class="settings-empty">Settings are available in the desktop app.</p>
  {:else}
    <div class="settings-shell">
      <nav class="settings-rail" aria-label="Settings pages">
        {#each pages as entry (entry.id)}
          <button class:active={page === entry.id} aria-current={page === entry.id ? "page" : undefined} onclick={() => (page = entry.id)}>{entry.label}</button>
        {/each}
      </nav>
      <section class="settings-page">
        {#if loadError}
          <p class="settings-error" role="alert">{loadError}</p>
        {:else if loading}
          <p class="settings-empty">Loading…</p>
        {:else if page === "endpoints"}
          <EndpointsPage {transport} {endpoints} {tiers} onendpoints={refreshAfterEndpointChange} />
        {:else}
          <TiersPage {transport} {endpoints} {tiers} ontiers={(next) => (tiers = next)} />
        {/if}
      </section>
    </div>
  {/if}
</Sheet>

<style>
  .settings-shell { display: grid; min-height: 0; flex: 1; grid-template-columns: 140px minmax(0, 1fr); }
  .settings-rail { display: flex; flex-direction: column; gap: 2px; padding: 12px 10px; border-right: 1px solid var(--line); background: var(--canvas); }
  .settings-rail button { padding: 7px 10px; border: 0; border-radius: 8px; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-body); font-weight: 600; text-align: left; }
  .settings-rail button:hover { color: var(--ink); }
  .settings-rail button.active { background: var(--surface); box-shadow: 0 1px 3px rgb(35 38 30 / 8%); color: var(--ink); }
  .settings-page { display: flex; min-width: 0; min-height: 0; flex-direction: column; }
  .settings-empty { margin: 0; padding: 28px 24px; color: var(--muted); font-size: var(--text-body); }
  .settings-error { margin: 0; padding: 20px 24px; color: var(--danger); font-size: var(--text-body); }
  @media (max-width: 640px) { .settings-shell { grid-template-columns: 1fr; grid-template-rows: auto 1fr; } .settings-rail { flex-direction: row; border-right: 0; border-bottom: 1px solid var(--line); } }
</style>
