<script lang="ts">
  import { untrack } from "svelte";
  import { createLayaTransport } from "../lib/layaSettings";
  import { createMcpTransport } from "../lib/mcpSettings";
  import { createModelSettingsTransport } from "../lib/modelSettings";
  import { createSafetyTransport } from "../lib/safetySettings";
  import { createAppearanceTransport } from "../lib/sounds";
  import { currentProject } from "../lib/store";
  import { createSettingsTransport, emptyTiers, type EndpointDetailDto, type TiersDto } from "../lib/settingsEngine";
  import { errorText } from "../lib/settingsState";
  import AppearancePage from "./AppearancePage.svelte";
  import EndpointsPage from "./EndpointsPage.svelte";
  import McpPage from "./McpPage.svelte";
  import ModelsPage from "./ModelsPage.svelte";
  import ProjectPage from "./ProjectPage.svelte";
  import SafetyPage from "./SafetyPage.svelte";
  import SavingsPage from "./SavingsPage.svelte";
  import Sheet from "./Sheet.svelte";
  import TiersPage from "./TiersPage.svelte";

  type Page =
    | "project"
    | "endpoints"
    | "tiers"
    | "models"
    | "savings"
    | "safety"
    | "mcp"
    | "appearance";

  /**
   * The rail, in two groups: the open project's own settings first, then every
   * page that spans the whole install. Safety and MCP keep their per-project
   * sections inside the install-wide pages.
   */
  const railGroups: { label: string; pages: { id: Page; label: string }[] }[] = [
    { label: "This project", pages: [{ id: "project", label: "Project" }] },
    {
      label: "All projects",
      pages: [
        { id: "endpoints", label: "Endpoints" },
        { id: "tiers", label: "Tiers" },
        { id: "models", label: "Models" },
        { id: "savings", label: "Savings" },
        { id: "safety", label: "Safety" },
        { id: "mcp", label: "MCP" },
        { id: "appearance", label: "Appearance" },
      ],
    },
  ];

  let {
    open,
    onclose,
    /** The page to show on open; the top bar's gear asks for "project". */
    initialPage = "endpoints",
  }: { open: boolean; onclose: () => void; initialPage?: Page | "laya" } = $props();

  const hasProject = $derived($currentProject !== null);
  /** The Project page wears the project’s name; "Project" stands in without one. */
  const projectLabel = $derived($currentProject?.name ?? "Project");

  /** The page an open request lands on — a Project request needs a project,
   * and the retired Decisions page redirects to Tiers (its Laya section). */
  function startPage(requested: Page | "laya"): Page {
    if (requested === "laya") return "tiers";
    return requested === "project" && !hasProject ? "endpoints" : requested;
  }

  // Read once for the first paint; the effect below re-resolves it on every open.
  let page = $state<Page>(untrack(() => startPage(initialPage)));

  // Re-resolved on every open, not just at mount, so the gear always lands on
  // the Project page while a later caller can ask for another one.
  $effect(() => {
    if (open) page = startPage(initialPage);
  });

  let endpoints = $state<EndpointDetailDto[]>([]);
  let tiers = $state<TiersDto>(emptyTiers());
  let loading = $state(false);
  let loadError = $state("");
  const transport = $derived(open ? createSettingsTransport() : null);
  const modelTransport = $derived(open ? createModelSettingsTransport() : null);
  const layaTransport = $derived(open ? createLayaTransport() : null);
  const safetyTransport = $derived(open ? createSafetyTransport() : null);
  const mcpTransport = $derived(open ? createMcpTransport() : null);
  const appearanceTransport = $derived(open ? createAppearanceTransport() : null);

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
        {#each railGroups as group (group.label)}
          <div class="rail-group">
            <p class="rail-heading">{group.label}</p>
            {#each group.pages as entry (entry.id)}
              <button class:active={page === entry.id} aria-current={page === entry.id ? "page" : undefined} onclick={() => (page = entry.id)}>{entry.id === "project" ? projectLabel : entry.label}</button>
            {/each}
          </div>
        {/each}
      </nav>
      <section class="settings-page">
        {#if page === "project"}
          <ProjectPage />
        {:else if loadError}
          <p class="settings-error" role="alert">{loadError}</p>
        {:else if loading}
          <p class="settings-empty">Loading…</p>
        {:else if page === "endpoints"}
          <EndpointsPage {transport} {endpoints} {tiers} onendpoints={refreshAfterEndpointChange} />
        {:else if page === "models"}
          {#if modelTransport}
            <ModelsPage transport={modelTransport} {endpoints} />
          {:else}
            <p class="settings-empty">Model settings are available in the desktop app.</p>
          {/if}
        {:else if page === "savings"}
          {#if modelTransport}
            <SavingsPage transport={modelTransport} {endpoints} />
          {:else}
            <p class="settings-empty">Savings settings are available in the desktop app.</p>
          {/if}
        {:else if page === "safety"}
          {#if safetyTransport}
            <SafetyPage transport={safetyTransport} />
          {:else}
            <p class="settings-empty">Safety settings are available in the desktop app.</p>
          {/if}
        {:else if page === "mcp"}
          {#if mcpTransport}
            <McpPage transport={mcpTransport} />
          {:else}
            <p class="settings-empty">MCP server settings are available in the desktop app.</p>
          {/if}
        {:else if page === "appearance"}
          {#if appearanceTransport}
            <AppearancePage transport={appearanceTransport} />
          {:else}
            <p class="settings-empty">Sound and theme settings are available in the desktop app.</p>
          {/if}
        {:else}
          <TiersPage {transport} {layaTransport} {endpoints} {tiers} ontiers={(next) => (tiers = next)} />
        {/if}
      </section>
    </div>
  {/if}
</Sheet>

<style>
  .settings-shell { display: grid; min-height: 0; flex: 1; grid-template-columns: 140px minmax(0, 1fr); }
  .settings-rail { display: flex; flex-direction: column; gap: 12px; padding: 12px 10px; border-right: 1px solid var(--line); background: var(--canvas); overflow-y: auto; }
  .rail-group { display: flex; flex-direction: column; gap: 2px; }
  .rail-heading { margin: 0 0 2px; padding: 0 10px; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.04em; text-transform: uppercase; }
  .settings-rail button { padding: 7px 10px; border: 0; border-radius: 8px; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-body); font-weight: 600; text-align: left; }
  .settings-rail button:hover { color: var(--ink); }
  .settings-rail button.active { background: var(--surface); box-shadow: var(--shadow-chip); color: var(--ink); }
  .settings-page { display: flex; min-width: 0; min-height: 0; flex-direction: column; }
  .settings-empty { margin: 0; padding: 28px 24px; color: var(--muted); font-size: var(--text-body); }
  .settings-error { margin: 0; padding: 20px 24px; color: var(--danger); font-size: var(--text-body); }
  @media (max-width: 640px) { .settings-shell { grid-template-columns: 1fr; grid-template-rows: auto 1fr; } .settings-rail { flex-direction: row; border-right: 0; border-bottom: 1px solid var(--line); overflow-x: auto; } }
</style>
