<script lang="ts">
  import { ArrowLeft, ArrowRight, Globe, Lock, RotateCw, ShieldCheck } from "@lucide/svelte";
  import { actions, app, currentProject } from "../lib/store";

  let address = $state("");

  $effect(() => {
    address = $app.browserUrl;
  });

  const isLocal = $derived(
    /^https?:\/\/(localhost|127\.0\.0\.1):5180/i.test($app.browserUrl),
  );

  function navigate(event: SubmitEvent) {
    event.preventDefault();
    actions.navigateBrowser(address);
  }

  function setSlots(event: Event) {
    actions.setWorkerSlots(Number((event.currentTarget as HTMLInputElement).value));
  }
</script>

<section class="browser-tab" aria-label="Mock browser">
  <div class="browser-chrome">
    <div class="chrome-buttons">
      <button class="chrome-button" aria-label="Back" title="Mock history · Phase 0" disabled><ArrowLeft size={13} /></button>
      <button class="chrome-button" aria-label="Forward" title="Mock history · Phase 0" disabled><ArrowRight size={13} /></button>
      <button class="chrome-button" aria-label="Reload" title="Mock reload · Phase 0" disabled><RotateCw size={12} /></button>
    </div>
    <form class="address-form" onsubmit={navigate}>
      <span class="address-lock">{#if isLocal}<Lock size={11} />{:else}<Globe size={11} />{/if}</span>
      <input aria-label="Address" bind:value={address} spellcheck="false" />
      <button type="submit" class="go-button">Go</button>
    </form>
    <span class="mock-chip">MOCK</span>
  </div>

  <div class="browser-page">
    {#if isLocal}
      <div class="page-nav"><span class="page-mark">M</span><span>Mnemo2</span><span class="page-nav-note">PROJECT SETTINGS · MOCK PREVIEW</span></div>
      <div class="page-layout">
        <aside class="page-aside">
          <span class="aside-active">Project</span>
          <span>Team</span>
          <span>Models</span>
          <span>Merge policy</span>
        </aside>
        <main class="page-main">
          <p class="page-eyebrow">PROJECT / SETTINGS</p>
          <h2>{$currentProject?.name ?? "Project"}</h2>
          <p class="page-copy">A single-project instance. Worker concurrency is set per project; budget and rate limits stay separate.</p>

          <div class="setting-row">
            <div><strong>Worker slots</strong><small>Maximum concurrent workers in this project.</small></div>
            <input aria-label="Settings worker slots" type="number" min="1" step="1" value={$currentProject?.workerSlots ?? 1} onchange={setSlots} />
          </div>
          <div class="setting-row">
            <div><strong>Default worker model</strong><small>Cheaper workers, strong lead.</small></div>
            <span class="setting-value">Spark Mini</span>
          </div>
          <div class="setting-row">
            <div><strong>Merge policy</strong><small>Serialized into the lead’s local branch after checks and review close.</small></div>
            <span class="setting-value setting-ok"><ShieldCheck size={12} /> Guarded</span>
          </div>
          <div class="setting-row">
            <div><strong>Budget</strong><small>${$currentProject?.budgetSpent.toFixed(2)} of ${$currentProject?.budgetLimit} used this project.</small></div>
            <span class="setting-value">${$currentProject?.budgetSpent.toFixed(2)} / ${$currentProject?.budgetLimit}</span>
          </div>
          <p class="page-footnote">This is a styled stand-in. Phase 4 embeds the native WebView2 pane over this slot; an iframe would not be a real browser.</p>
        </main>
      </div>
    {:else}
      <div class="external-page">
        <span class="external-icon"><Globe size={22} /></span>
        <p class="page-eyebrow">EXTERNAL PAGE</p>
        <h2>{$app.browserUrl.replace(/^https?:\/\//, "").split("/")[0]}</h2>
        <code>{$app.browserUrl}</code>
        <p class="page-copy">A link from the lead chat opened this address. Phase 0 shows a stand-in page; the native browser pane arrives with the Mnemo2 shell.</p>
        <button class="button button-quiet" onclick={() => actions.navigateBrowser("http://localhost:5180/settings")}><Lock size={12} /> Back to project settings</button>
      </div>
    {/if}
  </div>
</section>

<style>
  .browser-tab { display: flex; height: 100%; min-height: 0; flex-direction: column; }
  .browser-chrome { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-bottom: 1px solid var(--line); background: var(--surface-soft); }
  .chrome-buttons { display: flex; gap: 2px; }
  .chrome-button { display: grid; width: 24px; height: 24px; place-items: center; border: 0; border-radius: 6px; background: transparent; color: var(--muted); }
  .chrome-button:disabled { opacity: 0.5; }
  .address-form { display: flex; flex: 1; min-width: 0; align-items: center; gap: 4px; padding: 3px 3px 3px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); }
  .address-lock { display: inline-flex; color: var(--muted); }
  .address-form input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; color: var(--ink-soft); font-family: var(--font-mono); font-size: 8px; }
  .go-button { padding: 4px 7px; border: 0; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); cursor: pointer; font-size: 8px; }
  .mock-chip { padding: 3px 5px; border-radius: 4px; background: var(--warning-soft); color: var(--warning); font-size: 6px; font-weight: 750; letter-spacing: 0.08em; }
  .browser-page { flex: 1; overflow-y: auto; background: #fbfbf9; }
  .page-nav { display: flex; align-items: center; gap: 8px; padding: 13px 22px; border-bottom: 1px solid #eceded; background: white; color: var(--ink); font-size: 10px; font-weight: 650; }
  .page-mark { display: grid; width: 22px; height: 22px; place-items: center; border: 1px solid var(--line); border-radius: 6px; font-family: var(--font-prose); font-size: 13px; }
  .page-nav-note { margin-left: auto; color: var(--muted); font-size: 6px; letter-spacing: 0.1em; }
  .page-layout { display: grid; grid-template-columns: 120px 1fr; }
  .page-aside { display: grid; align-content: start; gap: 2px; padding: 20px 10px; border-right: 1px solid #eceee9; color: #858a7f; font-size: 8px; }
  .page-aside span { padding: 7px 9px; border-radius: 6px; }
  .page-aside .aside-active { background: #eef1e9; color: var(--accent-strong); font-weight: 650; }
  .page-main { padding: 26px 24px; }
  .page-eyebrow { margin: 0 0 5px; color: var(--muted); font-size: 6px; letter-spacing: 0.11em; }
  .page-main h2, .external-page h2 { margin: 0 0 8px; font-family: var(--font-prose); font-size: 25px; font-weight: 550; }
  .page-copy { max-width: 360px; margin: 0 0 22px; color: #757a6f; font-family: var(--font-prose); font-size: 12px; line-height: 1.55; }
  .setting-row { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 13px 0; border-top: 1px solid #eceee9; }
  .setting-row strong, .setting-row small { display: block; }
  .setting-row strong { font-size: 9px; font-weight: 650; }
  .setting-row small { margin-top: 3px; max-width: 260px; color: var(--muted); font-size: 7px; line-height: 1.4; }
  .setting-row input { width: 48px; padding: 5px; border: 1px solid var(--line); border-radius: 6px; background: white; color: var(--ink); font-size: 9px; text-align: center; }
  .setting-value { display: inline-flex; align-items: center; gap: 4px; flex: 0 0 auto; padding: 4px 7px; border-radius: 5px; background: #f1f2ee; color: #5f6459; font-size: 7px; }
  .setting-ok { color: var(--accent-strong); }
  .page-footnote { margin-top: 22px; color: #94998e; font-size: 7px; line-height: 1.5; }
  .external-page { display: grid; justify-items: center; padding: 60px 28px; text-align: center; }
  .external-icon { display: grid; width: 52px; height: 52px; margin-bottom: 18px; place-items: center; border: 1px solid var(--line); border-radius: 15px; background: white; color: var(--accent-strong); }
  .external-page code { margin: 0 0 18px; color: var(--muted); font-family: var(--font-mono); font-size: 8px; }
  @media (max-width: 560px) { .page-layout { grid-template-columns: 1fr; } .page-aside { display: none; } .page-main { padding: 20px 18px; } }
</style>
