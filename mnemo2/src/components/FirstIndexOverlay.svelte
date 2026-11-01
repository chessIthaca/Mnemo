<script lang="ts">
  import { DatabaseZap } from "@lucide/svelte";
  import { actions, app, currentProject } from "../lib/store";
  import { indexingHoldsProject } from "../lib/indexingEngine";

  const code = $derived($app.indexing?.code ?? null);
  const share = $derived(code && code.total > 0 ? Math.min(1, code.done / code.total) : null);
  const memory = $derived($app.indexing?.memory ?? null);
  const holding = $derived(indexingHoldsProject($app.indexing));

  let layer = $state<HTMLDivElement | null>(null);

  // The native Browser pane draws above HTML, and the keyboard must not reach
  // the workspace behind the card: hide the one, make the other inert.
  $effect(() => {
    if (!holding || !layer) return;
    actions.browserOverlay(true);
    const siblings = [...(layer.parentElement?.children ?? [])].filter(
      (element): element is HTMLElement => element instanceof HTMLElement && element !== layer,
    );
    for (const element of siblings) element.setAttribute("inert", "");
    return () => {
      for (const element of siblings) element.removeAttribute("inert");
      actions.browserOverlay(false);
    };
  });
</script>

{#if holding}
  <div class="first-index" role="dialog" aria-modal="true" aria-labelledby="first-index-title" bind:this={layer}>
    <div class="card">
      <span class="icon"><DatabaseZap size={18} /></span>
      <h2 id="first-index-title">Getting {$currentProject?.name ?? "this project"} ready</h2>
      <p>
        Mnemo is indexing the source code and memory for the first time, so the lead and its workers
        can search them. This happens once per project.
      </p>
      <div
        class="bar"
        class:counting={share === null}
        role="progressbar"
        aria-label="First code index progress"
        aria-valuemin="0"
        aria-valuemax="100"
        aria-valuenow={share === null ? undefined : Math.round(share * 100)}
      ><span style:width={share === null ? undefined : `${share * 100}%`}></span></div>
      <div class="counter">
        {#if code && code.total > 0}
          {code.done.toLocaleString()} / {code.total.toLocaleString()} files
        {:else}
          Counting files…
        {/if}
        {#if memory?.state === "running"}
          <span>· updating memory</span>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .first-index { position: fixed; z-index: 40; inset: 0; display: grid; place-items: center; padding: 16px; background: rgb(247 247 244 / 82%); backdrop-filter: blur(3px); }
  .card { width: min(420px, 100%); padding: 22px 24px 20px; border: 1px solid var(--line); border-radius: 16px; background: var(--surface); box-shadow: var(--shadow); }
  .icon { display: grid; width: 34px; height: 34px; place-items: center; border-radius: 10px; background: var(--accent-soft); color: var(--accent-strong); }
  h2 { margin: 14px 0 6px; font-size: var(--text-title); font-weight: 650; }
  p { margin: 0 0 18px; color: var(--ink-soft); font-size: var(--text-body); line-height: 1.55; }
  .bar { position: relative; height: 4px; overflow: hidden; border-radius: 2px; background: var(--line); }
  .bar > span { position: absolute; inset: 0 auto 0 0; border-radius: inherit; background: var(--accent); transition: width 240ms ease; }
  .bar.counting > span { width: 30%; animation: sweep 1.3s ease-in-out infinite; }
  .counter { margin-top: 8px; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-body); font-variant-numeric: tabular-nums; }
  @keyframes sweep { from { left: -30%; } to { left: 100%; } }
  @media (prefers-reduced-motion: reduce) { .bar.counting > span { animation: none; left: 35%; } }
</style>
