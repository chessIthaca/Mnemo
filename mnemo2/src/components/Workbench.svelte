<script lang="ts">
  import { Activity, ArrowLeft, Brain, FileDiff, FolderTree, GitBranch, Globe, ListTree, Network, X } from "@lucide/svelte";
  import { onMount, type Component } from "svelte";
  import { actions, app, isWideWorkbenchTab, workbenchPanelWidth } from "../lib/store";
  import type { WorkbenchTab } from "../lib/types";
  import BrowserTab from "./BrowserTab.svelte";
  import DiffTab from "./DiffTab.svelte";
  import FilesTab from "./FilesTab.svelte";
  import GitTab from "./GitTab.svelte";
  import MemoryView from "./MemoryView.svelte";
  import PlanTab from "./PlanTab.svelte";

  const tabs: { id: WorkbenchTab; label: string; icon: Component }[] = [
    { id: "browser", label: "Browser", icon: Globe },
    { id: "diff", label: "Diff", icon: FileDiff },
    { id: "plan", label: "Plan", icon: ListTree },
    { id: "files", label: "Files", icon: FolderTree },
    { id: "git", label: "Git", icon: GitBranch },
    { id: "trace", label: "Trace", icon: Activity },
    { id: "graph", label: "Graph", icon: Network },
    { id: "memory", label: "Memory", icon: Brain },
  ];

  let resizing = $state(false);
  let windowWidth = $state(window.innerWidth);
  const wide = $derived(isWideWorkbenchTab($app.workbenchTab));
  const panelWidth = $derived(workbenchPanelWidth($app.workbenchTab, $app.workbenchWidth, $app.workbenchWideWidth, windowWidth));

  /** Drag whichever width the open tab owns; the other tab kind keeps its own. */
  function resizeTo(width: number) {
    if (wide) actions.setWideWorkbenchWidth(width);
    else actions.setWorkbenchWidth(width);
  }

  function startResize(event: PointerEvent) {
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    resizing = true;
  }

  function handleMove(event: PointerEvent) {
    if (!resizing) return;
    resizeTo(window.innerWidth - event.clientX);
  }

  function stopResize() {
    resizing = false;
  }

  function handleWindowResize() {
    windowWidth = window.innerWidth;
    actions.reclampWorkbenchWidth();
  }

  // A saved wide panel yields to the main tab when the window cannot hold both.
  onMount(() => actions.reclampWorkbenchWidth());

  function resizeKeydown(event: KeyboardEvent) {
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      resizeTo(panelWidth + 24);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      resizeTo(panelWidth - 24);
    }
  }
</script>

<svelte:window onpointermove={handleMove} onpointerup={stopResize} onpointercancel={stopResize} onresize={handleWindowResize} />

<aside class="workbench" style={`width: ${panelWidth}px`} aria-label="Workbench">
  <button
    class="resizer"
    class:resizing
    type="button"
    aria-label="Resize workbench"
    title={wide ? "Drag or use the arrow keys to resize; double-click to reset" : "Drag or use the arrow keys to resize"}
    onpointerdown={startResize}
    onkeydown={resizeKeydown}
    ondblclick={() => { if (wide) actions.setWideWorkbenchWidth(null); }}
  ></button>

  <div class="bench-frame">
    <header class="bench-header">
      <button class="back-button" aria-label="Close workbench" onclick={actions.closeWorkbench}><ArrowLeft size={15} /></button>
      <nav class="bench-tabs" aria-label="Workbench tabs">
        {#each tabs as tab (tab.id)}
          {@const Icon = tab.icon}
          <button
            class:active={$app.workbenchTab === tab.id}
            aria-pressed={$app.workbenchTab === tab.id}
            aria-label={tab.label}
            title={tab.label}
            onclick={() => actions.setWorkbenchTab(tab.id)}
          ><Icon size={13} /><span class="tab-label">{tab.label}</span></button>
        {/each}
      </nav>
      <button class="close-button" aria-label="Close workbench" onclick={actions.closeWorkbench}><X size={15} /></button>
    </header>

    <div class="bench-body" class:fill={wide}>
      {#if $app.workbenchTab === "browser"}
        <BrowserTab obscured={resizing} />
      {:else if $app.workbenchTab === "diff"}
        <DiffTab />
      {:else if $app.workbenchTab === "files"}
        <FilesTab />
      {:else if $app.workbenchTab === "git"}
        <GitTab />
      {:else if $app.workbenchTab === "trace"}
        {#await import("./TraceView.svelte") then { default: TraceView }}
          <TraceView />
        {:catch error}
          <p class="view-load-error">Could not load this view: {String(error)}</p>
        {/await}
      {:else if $app.workbenchTab === "graph"}
        {#await import("./GraphView.svelte") then { default: GraphView }}
          <GraphView />
        {:catch error}
          <p class="view-load-error">Could not load this view: {String(error)}</p>
        {/await}
      {:else if $app.workbenchTab === "memory"}
        <MemoryView />
      {:else}
        <PlanTab scope="open" />
      {/if}
    </div>
  </div>
</aside>

<style>
  .workbench { position: relative; display: flex; flex: 0 0 auto; min-width: 0; min-height: 0; flex-direction: column; border-left: 1px solid var(--line); background: var(--surface); }
  .resizer { position: absolute; top: 0; bottom: 0; left: -4px; z-index: 3; width: 8px; padding: 0; border: 0; background: transparent; cursor: col-resize; touch-action: none; }
  .resizer::after { position: absolute; top: 0; bottom: 0; left: 3px; width: 2px; background: transparent; content: ""; transition: background 140ms ease; }
  .resizer:hover::after, .resizer.resizing::after, .resizer:focus-visible::after { background: var(--accent); }
  .bench-frame { display: flex; flex: 1; min-width: 0; min-height: 0; flex-direction: column; container: bench-panel / inline-size; }
  .bench-header { display: flex; align-items: center; gap: 10px; min-height: 50px; padding: 0 12px; border-bottom: 1px solid var(--line); }
  .back-button { display: none; }
  .bench-tabs { display: flex; flex: 1; gap: 4px; min-width: 0; overflow-x: auto; scrollbar-width: none; }
  .bench-tabs button { display: inline-flex; align-items: center; gap: 5px; flex: 0 0 auto; padding: 6px 9px; border: 1px solid transparent; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-body); font-weight: 600; }
  .bench-tabs button.active { border-color: var(--line); background: var(--surface-soft); color: var(--ink); }
  .tab-label { white-space: nowrap; }
  .close-button { display: grid; width: 28px; height: 28px; place-items: center; border: 0; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; }
  .close-button:hover { background: var(--surface-soft); color: var(--ink); }
  .bench-body { flex: 1; min-height: 0; overflow-y: auto; }
  .bench-body.fill { display: flex; overflow: hidden; }
  .view-load-error { margin: 24px; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  @container bench-panel (max-width: 700px) {
    .bench-tabs button:not(.active) .tab-label { display: none; }
  }
  @media (max-width: 980px) {
    .workbench { position: absolute; inset: 0; z-index: 30; width: 100% !important; border-left: 0; box-shadow: var(--shadow); }
    .resizer { display: none; }
    .back-button { display: grid; width: 28px; height: 28px; place-items: center; border: 0; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; }
  }
</style>
