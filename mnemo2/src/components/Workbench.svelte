<script lang="ts">
  import { ArrowLeft, FileDiff, Globe, ListTree, X } from "@lucide/svelte";
  import type { Component } from "svelte";
  import { actions, app } from "../lib/store";
  import type { WorkbenchTab } from "../lib/types";
  import BrowserTab from "./BrowserTab.svelte";
  import DiffTab from "./DiffTab.svelte";
  import PlanTab from "./PlanTab.svelte";

  const tabs: { id: WorkbenchTab; label: string; icon: Component }[] = [
    { id: "browser", label: "Browser", icon: Globe },
    { id: "diff", label: "Diff", icon: FileDiff },
    { id: "plan", label: "Plan", icon: ListTree },
  ];

  let resizing = $state(false);

  function startResize(event: PointerEvent) {
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    resizing = true;
  }

  function handleMove(event: PointerEvent) {
    if (!resizing) return;
    actions.setWorkbenchWidth(window.innerWidth - event.clientX);
  }

  function stopResize() {
    resizing = false;
  }

  function resizeKeydown(event: KeyboardEvent) {
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      actions.setWorkbenchWidth($app.workbenchWidth + 24);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      actions.setWorkbenchWidth($app.workbenchWidth - 24);
    }
  }
</script>

<svelte:window onpointermove={handleMove} onpointerup={stopResize} onpointercancel={stopResize} />

<aside class="workbench" style={`width: ${$app.workbenchWidth}px`} aria-label="Workbench">
  <button
    class="resizer"
    class:resizing
    type="button"
    aria-label="Resize workbench"
    title="Drag or use the arrow keys to resize"
    onpointerdown={startResize}
    onkeydown={resizeKeydown}
  ></button>

  <header class="bench-header">
    <button class="back-button" aria-label="Close workbench" onclick={actions.closeWorkbench}><ArrowLeft size={15} /></button>
    <nav class="bench-tabs" aria-label="Workbench tabs">
      {#each tabs as tab (tab.id)}
        {@const Icon = tab.icon}
        <button
          class:active={$app.workbenchTab === tab.id}
          aria-pressed={$app.workbenchTab === tab.id}
          onclick={() => actions.setWorkbenchTab(tab.id)}
        ><Icon size={13} /> {tab.label}</button>
      {/each}
    </nav>
    <button class="close-button" aria-label="Close workbench" onclick={actions.closeWorkbench}><X size={15} /></button>
  </header>

  <div class="bench-body">
    {#if $app.workbenchTab === "browser"}
      <BrowserTab obscured={resizing} />
    {:else if $app.workbenchTab === "diff"}
      <DiffTab />
    {:else}
      <PlanTab />
    {/if}
  </div>
</aside>

<style>
  .workbench { position: relative; display: flex; flex: 0 0 auto; min-width: 0; min-height: 0; flex-direction: column; border-left: 1px solid var(--line); background: var(--surface); }
  .resizer { position: absolute; top: 0; bottom: 0; left: -4px; z-index: 3; width: 8px; padding: 0; border: 0; background: transparent; cursor: col-resize; touch-action: none; }
  .resizer::after { position: absolute; top: 0; bottom: 0; left: 3px; width: 2px; background: transparent; content: ""; transition: background 140ms ease; }
  .resizer:hover::after, .resizer.resizing::after, .resizer:focus-visible::after { background: var(--accent); }
  .bench-header { display: flex; align-items: center; gap: 10px; min-height: 50px; padding: 0 12px; border-bottom: 1px solid var(--line); }
  .back-button { display: none; }
  .bench-tabs { display: flex; flex: 1; gap: 4px; }
  .bench-tabs button { display: inline-flex; align-items: center; gap: 5px; padding: 6px 9px; border: 1px solid transparent; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-body); font-weight: 600; }
  .bench-tabs button.active { border-color: var(--line); background: var(--surface-soft); color: var(--ink); }
  .close-button { display: grid; width: 28px; height: 28px; place-items: center; border: 0; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; }
  .close-button:hover { background: var(--surface-soft); color: var(--ink); }
  .bench-body { flex: 1; min-height: 0; overflow-y: auto; }
  @media (max-width: 980px) {
    .workbench { position: absolute; inset: 0; z-index: 30; width: 100% !important; border-left: 0; box-shadow: var(--shadow); }
    .resizer { display: none; }
    .back-button { display: grid; width: 28px; height: 28px; place-items: center; border: 0; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; }
  }
</style>
