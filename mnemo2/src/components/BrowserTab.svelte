<script lang="ts">
  import { ArrowLeft, ArrowRight, ExternalLink, Globe, RotateCw, X } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { actions, app } from "../lib/store";

  let { obscured = false }: { obscured?: boolean } = $props();
  let surface = $state<HTMLDivElement | null>(null);
  let address = $state("");
  let lastBrowserUrl = "";
  let lastVisibilityKey = "";
  let frame: number | null = null;

  const hasPage = $derived(/^https?:\/\//i.test($app.browserUrl));
  const nativeVisible = $derived(
    !obscured &&
      $app.browserSupported === true &&
      hasPage &&
      !$app.browserImage &&
      $app.workbenchOpen &&
      $app.workbenchTab === "browser",
  );

  $effect(() => {
    const url = $app.browserUrl;
    if (url !== lastBrowserUrl) {
      lastBrowserUrl = url;
      address = url;
    }
  });

  $effect(() => {
    const key = `${$app.browserSupported}|${nativeVisible}|${$app.browserUrl}`;
    if (key === lastVisibilityKey) return;
    lastVisibilityKey = key;
    const visible = nativeVisible;
    if ($app.browserSupported === true) void actions.setBrowserVisible(visible);
    if (visible) void ensurePane();
  });

  onMount(() => {
    let active = true;
    void actions.connectBrowser().then((transport) => {
      if (!active) return;
      if (!transport) {
        app.update((state) => ({
          ...state,
          browserSupported: false,
          browserError: "The native Mnemo2 shell is required for embedded browsing.",
        }));
        return;
      }
    });
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(scheduleRect);
    if (surface) observer?.observe(surface);
    window.addEventListener("resize", scheduleRect);
    return () => {
      active = false;
      observer?.disconnect();
      window.removeEventListener("resize", scheduleRect);
      if (frame !== null) cancelAnimationFrame(frame);
      void actions.setBrowserVisible(false);
    };
  });

  function scheduleRect() {
    if (frame !== null) return;
    frame = requestAnimationFrame(() => {
      frame = null;
      if (!surface || !nativeVisible) return;
      const bounds = surface.getBoundingClientRect();
      if (bounds.width < 1 || bounds.height < 1) return;
      const scale = window.devicePixelRatio || 1;
      void actions.setBrowserRect({
        x: Math.round(bounds.x * scale),
        y: Math.round(bounds.y * scale),
        width: Math.max(1, Math.round(bounds.width * scale)),
        height: Math.max(1, Math.round(bounds.height * scale)),
      });
    });
  }

  async function ensurePane(): Promise<void> {
    if (!surface || !nativeVisible) return;
    const bounds = surface.getBoundingClientRect();
    if (bounds.width < 1 || bounds.height < 1) return;
    const scale = window.devicePixelRatio || 1;
    await actions.ensureBrowser({
      x: Math.round(bounds.x * scale),
      y: Math.round(bounds.y * scale),
      width: Math.max(1, Math.round(bounds.width * scale)),
      height: Math.max(1, Math.round(bounds.height * scale)),
    }, $app.browserUrl);
  }

  function navigate(event: SubmitEvent) {
    event.preventDefault();
    void actions.navigateBrowser(address);
  }
</script>

<section class="browser-tab" aria-label="Browser">
  <div class="browser-chrome">
    <div class="chrome-buttons">
      <button class="chrome-button" aria-label="Back" title="Back" disabled={!hasPage} onclick={actions.browserBack}><ArrowLeft size={14} /></button>
      <button class="chrome-button" aria-label="Forward" title="Forward" disabled={!hasPage} onclick={actions.browserForward}><ArrowRight size={14} /></button>
      <button class="chrome-button" aria-label="Reload" title="Reload" disabled={!hasPage} onclick={actions.browserReload}><RotateCw size={13} /></button>
    </div>
    <form class="address-form" onsubmit={navigate}>
      <Globe size={12} />
      <input aria-label="Address" bind:value={address} spellcheck="false" placeholder="https:// or localhost:3000" />
      <button type="submit" class="go-button">Go</button>
    </form>
    <button class="chrome-button" aria-label="Open in system browser" title="Open in system browser" disabled={!hasPage} onclick={actions.openExternalBrowser}><ExternalLink size={13} /></button>
  </div>
  {#if $app.browserTitle}<div class="page-title">{$app.browserTitle}{#if $app.browserLoading && !$app.browserImage}<span>Loading…</span>{/if}</div>{/if}
  {#if $app.browserError}<div class="browser-error" role="alert">{$app.browserError}</div>{/if}

  <div class="browser-content">
    <div class:page-hidden={!nativeVisible} class="native-page" bind:this={surface} aria-label="Native browser page"></div>
    {#if $app.browserImage}
      <div class="screenshot-page">
        <header><strong>{$app.browserImage.label}</strong><button class="button button-quiet" onclick={actions.closeScreenshot}><X size={12} /> Close preview</button></header>
        {#if $app.browserImage.dataUrl}
          <img src={$app.browserImage.dataUrl} alt={$app.browserImage.label} />
        {:else}
          <p role="status">Reading the worker screenshot…</p>
        {/if}
      </div>
    {:else if !hasPage}
      <div class="browser-empty"><Globe size={24} /><h2>Open a page</h2><p>Enter an HTTP(S) address above, or choose a link in the conversation.</p></div>
    {:else if $app.browserSupported === false}
      <div class="browser-empty"><Globe size={24} /><h2>Open this page externally</h2><p>The embedded native pane is unavailable in this environment. Your system browser can open the same address.</p><button class="button button-primary" onclick={actions.openExternalBrowser}><ExternalLink size={13} /> Open in system browser</button></div>
    {:else if $app.browserSupported === null}
      <p class="browser-status" role="status">Connecting to the native browser…</p>
    {/if}
  </div>
</section>

<style>
  .browser-tab { display: flex; height: 100%; min-height: 0; flex-direction: column; }
  .browser-chrome { display: flex; align-items: center; gap: 7px; padding: 8px 10px; border-bottom: 1px solid var(--line); background: var(--surface-soft); }
  .chrome-buttons { display: flex; gap: 2px; }
  .chrome-button { display: grid; width: 27px; height: 27px; place-items: center; border: 0; border-radius: 6px; background: transparent; color: var(--muted); cursor: pointer; }
  .chrome-button:hover:not(:disabled) { background: var(--surface); color: var(--ink); }
  .chrome-button:disabled { opacity: 0.4; cursor: default; }
  .address-form { display: flex; flex: 1; min-width: 0; align-items: center; gap: 6px; padding: 3px 3px 3px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--muted); }
  .address-form input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; color: var(--ink-soft); font-family: var(--font-mono); font-size: 11px; }
  .go-button { padding: 5px 8px; border: 0; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); cursor: pointer; font-size: 10px; }
  .page-title { display: flex; justify-content: space-between; gap: 12px; padding: 7px 12px; border-bottom: 1px solid var(--line); color: var(--ink-soft); font-size: 11px; }
  .page-title span { color: var(--muted); font-size: 10px; }
  .browser-error { padding: 8px 12px; border-bottom: 1px solid #e3c7c0; background: #fbf0ee; color: #8d4d46; font-size: 11px; line-height: 1.5; }
  .browser-content { position: relative; flex: 1; min-height: 0; overflow: hidden; background: white; }
  .native-page { position: absolute; inset: 0; }
  .page-hidden { visibility: hidden; pointer-events: none; }
  .browser-empty { display: grid; height: 100%; align-content: center; justify-items: center; padding: 28px; color: var(--muted); text-align: center; }
  .browser-empty h2 { margin: 16px 0 8px; color: var(--ink); font-family: var(--font-prose); font-size: 25px; font-weight: 550; }
  .browser-empty p { max-width: 340px; margin: 0 0 20px; font-size: 12px; line-height: 1.65; }
  .browser-status { padding: 24px; color: var(--muted); font-size: 12px; }
  .screenshot-page { display: flex; height: 100%; min-height: 0; flex-direction: column; background: var(--surface-soft); }
  .screenshot-page header { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 8px 10px; border-bottom: 1px solid var(--line); font-size: 11px; }
  .screenshot-page header strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .screenshot-page img { flex: 1; min-height: 0; object-fit: contain; }
  .screenshot-page p { padding: 20px; color: var(--muted); font-size: 12px; }
  @media (max-width: 480px) { .browser-chrome { gap: 4px; padding: 7px; } .chrome-button { width: 25px; } .address-form input { font-size: 10px; } }
</style>
