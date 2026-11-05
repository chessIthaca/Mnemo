<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { createBoardTransport } from "../lib/boardEngine";
  import { specCapabilities } from "../lib/planDocument";
  import { isTauriRuntime } from "../lib/runtime";
  import { actions } from "../lib/store";
  import Markdown from "./Markdown.svelte";

  /**
   * The plan a worker works from, as its Plan tab shows it: the whole plan
   * document — goal, spec changes, acceptance scenarios, steps — with the
   * worker's step in the header, and below it the current OpenSpec spec of
   * every capability the plan's spec changes touch, folded and read on open.
   */
  let { planId, planStep }: { planId: string; planStep: number | null } = $props();

  const transport = createBoardTransport();

  let markdown = $state("");
  let loading = $state(false);
  let error = $state("");

  /** The capabilities the plan's `## Spec changes` section names, in order. */
  const capabilities = $derived(specCapabilities(markdown));

  /** One capability's current spec, read the first time its fold opens. */
  type SpecLoad =
    | { state: "loading" }
    | { state: "text"; markdown: string }
    | { state: "missing" }
    | { state: "unavailable" }
    | { state: "error"; message: string };

  let specLoads = $state<Record<string, SpecLoad | undefined>>({});

  /** Flatten a rejected invoke (string or Error) to display text. */
  function errorText(failure: unknown, fallback: string): string {
    return typeof failure === "string"
      ? failure
      : failure instanceof Error
        ? failure.message
        : fallback;
  }

  /**
   * Read the plan's document. A read that resolves for a planId the tab has
   * since moved on from is dropped, so the newer read wins.
   */
  async function reload(id: string): Promise<void> {
    const active = transport;
    if (!active) {
      loading = false;
      error = "Plan details need the desktop app.";
      return;
    }
    // A refresh of the plan already on screen keeps it (scroll position and
    // open spec folds survive) until the new text arrives; only a different
    // plan blanks the panel and shows the loading note.
    const refresh = id === shownId && markdown !== "";
    if (!refresh) {
      loading = true;
      error = "";
      markdown = "";
    }
    try {
      const text = await active.document(id);
      if (id !== planId) return;
      markdown = text;
      shownId = id;
      error = "";
      loading = false;
    } catch (failure) {
      if (id !== planId) return;
      loading = false;
      // A failed refresh leaves the shown plan in place.
      if (!refresh) error = errorText(failure, "Could not read the plan");
    }
  }

  /** The plan id whose document is on screen, so a refresh can keep it. */
  let shownId = "";

  // Mount and every planId change read the plan's document fresh.
  $effect(() => {
    void reload(planId);
  });

  // While the tab stays mounted, a board change re-reads the document, so
  // the plan follows edits made elsewhere in the app.
  onMount(() => {
    const active = transport;
    if (!active) return;
    let live = true;
    let unlisten: (() => void) | null = null;
    void active.onChanged(() => {
      if (live) void reload(planId);
    }).then((stop) => {
      if (live) unlisten = stop;
      else stop();
    });
    return () => {
      live = false;
      unlisten?.();
    };
  });

  /**
   * Read one capability's current spec the first time its fold opens. In a
   * plain browser (no Tauri host) the fold says so instead of calling the
   * command; null means the capability has no spec yet — this plan creates it.
   */
  function loadSpec(capability: string, open: boolean): void {
    if (!open || specLoads[capability]) return;
    if (!isTauriRuntime()) {
      specLoads[capability] = { state: "unavailable" };
      return;
    }
    specLoads[capability] = { state: "loading" };
    invoke<string | null>("read_capability_spec", { capability })
      .then((text) => {
        specLoads[capability] = text === null ? { state: "missing" } : { state: "text", markdown: text };
      })
      .catch((failure: unknown) => {
        specLoads[capability] = { state: "error", message: errorText(failure, "Could not read the spec") };
      });
  }
</script>

<section class="worker-plan" aria-label={`Plan ${planId}`}>
  <header class="plan-head">
    <h2 class="plan-heading">{planStep === null ? `Plan ${planId}` : `Plan ${planId} · step ${planStep + 1}`}</h2>
    <button
      class="toolbar-button"
      type="button"
      title="Open this plan in the Plans route"
      onclick={() => actions.openPlanDocument(planId)}
    >Open in Plans</button>
  </header>

  {#if loading}
    <p class="plan-note">Loading plan…</p>
  {:else if error}
    <p class="plan-note plan-error">{error}</p>
  {:else}
    <div class="plan-document">
      <Markdown source={markdown} onOpenLink={actions.openBrowser} onOpenPath={actions.openRepoPath} />
    </div>
    {#if capabilities.length > 0}
      <div class="current-specs">
        {#each capabilities as capability (capability)}
          {@const load = specLoads[capability]}
          <details class="current-spec" ontoggle={(event) => loadSpec(capability, event.currentTarget.open)}>
            <summary>Current spec: {capability}</summary>
            {#if load?.state === "loading"}
              <p class="spec-note">Loading spec…</p>
            {:else if load?.state === "unavailable"}
              <p class="spec-note">Specs need the desktop app.</p>
            {:else if load?.state === "missing"}
              <p class="spec-note">No current spec yet; this plan creates it.</p>
            {:else if load?.state === "error"}
              <p class="spec-note spec-error">{load.message}</p>
            {:else if load?.state === "text"}
              <div class="spec-doc">
                <Markdown source={load.markdown} onOpenLink={actions.openBrowser} onOpenPath={actions.openRepoPath} />
              </div>
            {/if}
          </details>
        {/each}
      </div>
    {/if}
  {/if}
</section>

<style>
  .worker-plan { min-width: 0; }
  .plan-head { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin: 0 0 10px; }
  .plan-heading { margin: 0; overflow: hidden; color: var(--ink); font-family: var(--font-ui); font-size: var(--text-title); font-weight: 650; letter-spacing: -0.01em; text-overflow: ellipsis; white-space: nowrap; }
  .toolbar-button { display: inline-flex; min-height: 23px; flex: 0 0 auto; align-items: center; justify-content: center; gap: 3px; padding: 2px 6px; border: 1px solid var(--line); border-radius: 5px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: var(--text-body); font-weight: 520; line-height: 1; }
  .toolbar-button:hover { border-color: var(--line-strong); background: var(--surface-soft); color: var(--ink); }
  .plan-note { margin: 0; color: var(--muted); font-size: var(--text-body); }
  .plan-error { color: var(--danger); }
  .plan-document { padding: 12px 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
  .plan-document :global(.markdown) { font-size: var(--text-body); line-height: 1.55; }
  .plan-document :global(.markdown h2) { margin: 16px 0 6px; font-size: var(--text-title); font-weight: 620; }
  .plan-document :global(.markdown h2:first-child) { margin-top: 0; }
  .plan-document :global(.markdown h3) { margin: 12px 0 4px; font-size: var(--text-body); font-weight: 650; }
  .current-specs { margin-top: 12px; }
  .current-spec { margin: 0 0 8px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); }
  .current-spec > summary { padding: 7px 11px; color: var(--muted); font-size: var(--text-body); cursor: pointer; user-select: none; }
  .current-spec[open] > summary { border-bottom: 1px solid var(--line); }
  .spec-note { margin: 0; padding: 8px 12px 10px; color: var(--muted); font-size: var(--text-body); }
  .spec-error { color: var(--danger); }
  .spec-doc { padding: 8px 12px 10px; }
  .spec-doc :global(.markdown) { font-size: var(--text-body); line-height: 1.55; }
</style>
