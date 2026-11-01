<script lang="ts">
  import { X } from "@lucide/svelte";
  import { onDestroy, tick } from "svelte";
  import type { Snippet } from "svelte";
  import { actions } from "../lib/store";

  let {
    open,
    title,
    eyebrow = "",
    wide = false,
    onclose,
    children,
  }: {
    open: boolean;
    title: string;
    eyebrow?: string;
    /** A large, edge-to-edge body for multi-pane dialogs. */
    wide?: boolean;
    onclose: () => void;
    children?: Snippet;
  } = $props();

  let sheetEl = $state<HTMLDivElement | null>(null);
  let closeButton = $state<HTMLButtonElement | null>(null);
  let returnFocus: HTMLElement | null = null;
  let inerted: HTMLElement[] = [];

  function setBackgroundInert(inert: boolean) {
    if (!inert) {
      for (const element of inerted) element.removeAttribute("inert");
      inerted = [];
      return;
    }
    const layer = document.querySelector(".sheet-layer");
    const shell = layer?.parentElement;
    inerted = Array.from(shell?.children ?? []).filter(
      (element): element is HTMLElement => element instanceof HTMLElement && element !== layer,
    );
    for (const element of inerted) element.setAttribute("inert", "");
  }

  $effect(() => {
    if (open) {
      actions.browserOverlay(true);
      returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      setBackgroundInert(true);
      void tick().then(() => closeButton?.focus());
      return () => actions.browserOverlay(false);
    }
    setBackgroundInert(false);
    returnFocus?.focus();
    returnFocus = null;
  });

  onDestroy(() => {
    setBackgroundInert(false);
    returnFocus?.focus();
  });

  function handleKeydown(event: KeyboardEvent) {
    if (!open) return;
    if (event.key === "Escape") {
      onclose();
      return;
    }
    if (event.key !== "Tab" || !sheetEl) return;
    const focusable = Array.from(
      sheetEl.querySelectorAll<HTMLElement>(
        'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((element) => element.offsetParent !== null);
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

{#if open}
  <div class="sheet-layer">
    <button class="sheet-backdrop" aria-label="Close sheet" tabindex="-1" onclick={onclose}></button>
    <div class="sheet" class:wide role="dialog" aria-modal="true" aria-label={title} bind:this={sheetEl}>
      <header>
        <div>
          {#if eyebrow}<p>{eyebrow}</p>{/if}
          <h2>{title}</h2>
        </div>
        <button class="icon-button" aria-label="Close" bind:this={closeButton} onclick={onclose}><X size={16} /></button>
      </header>
      <div class="sheet-body" class:wide>{@render children?.()}</div>
    </div>
  </div>
{/if}

<style>
  .sheet-layer { position: fixed; inset: 0; z-index: 60; }
  .sheet-backdrop { position: absolute; inset: 0; width: 100%; border: 0; background: rgb(35 38 30 / 18%); backdrop-filter: blur(2px); }
  .sheet { position: absolute; top: 50%; left: 50%; display: flex; width: min(480px, calc(100vw - 32px)); max-height: min(640px, calc(100vh - 48px)); flex-direction: column; overflow: hidden; border: 1px solid var(--line); border-radius: 18px; background: var(--surface); box-shadow: var(--shadow); transform: translate(-50%, -50%); }
  header { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; padding: 20px 20px 12px; }
  header p { margin: 0 0 4px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.11em; }
  header h2 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 550; }
  .sheet-body { overflow-y: auto; padding: 4px 20px 20px; }
  .sheet.wide { width: min(960px, calc(100vw - 32px)); height: min(680px, calc(100vh - 48px)); max-height: none; }
  .sheet.wide header { padding-bottom: 14px; border-bottom: 1px solid var(--line); }
  .sheet-body.wide { display: flex; min-height: 0; flex: 1; flex-direction: column; overflow: hidden; padding: 0; }
  @media (max-width: 560px) { .sheet { top: auto; bottom: 0; left: 0; width: 100%; max-height: 82vh; border-radius: 18px 18px 0 0; transform: none; } }
</style>
