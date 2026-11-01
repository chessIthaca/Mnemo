<script lang="ts">
  import { actions, app } from "../lib/store";
  import { formatTokenCount } from "../lib/usage";

  // Mnemo1's context colour thresholds (InflightBar contextColor): green
  // below half, amber below eighty percent, red from there. Neutral grey
  // while the window size is unknown.
  const RADIUS = 8.5;
  const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

  const runtime = $derived($app.leadRuntime);
  const hasWindow = $derived(runtime.maxTokens > 0);
  const pct = $derived(
    hasWindow ? Math.min(100, Math.round((runtime.usedTokens / runtime.maxTokens) * 100)) : 0,
  );
  const fill = $derived(hasWindow ? Math.min(1, runtime.usedTokens / runtime.maxTokens) : 0);
  const level = $derived(
    !hasWindow ? "level-unknown" : fill < 0.5 ? "level-ok" : fill < 0.8 ? "level-warn" : "level-critical",
  );
  const label = $derived(
    hasWindow
      ? `Lead context ${pct}% used (${formatTokenCount(runtime.usedTokens)} of ${formatTokenCount(runtime.maxTokens)} tokens)`
      : "Lead context usage unknown (window size unknown)",
  );
  const compactDisabled = $derived(
    runtime.busy || runtime.compacting || runtime.mode !== "native" || runtime.status === "opening",
  );

  function rolesOf(breakdown: NonNullable<typeof runtime.contextBreakdown>) {
    const values = [
      { role: "System", tokens: breakdown.system },
      { role: "User", tokens: breakdown.user },
      { role: "Assistant", tokens: breakdown.assistant },
      { role: "Tool", tokens: breakdown.tool },
    ];
    const peak = Math.max(...values.map((entry) => entry.tokens));
    return values.map((entry) => ({ ...entry, share: peak > 0 ? entry.tokens / peak : 0 }));
  }

  const roles = $derived(runtime.contextBreakdown ? rolesOf(runtime.contextBreakdown) : []);

  // The tip opens on hover/focus like TopBar's usage chip; a click pins it
  // open so its button can be reached. Esc or an outside click dismisses.
  let open = $state(false);
  let pinned = $state(false);
  let chip: HTMLElement | undefined = $state();
  let ringButton: HTMLButtonElement | undefined = $state();

  function show() {
    open = true;
  }

  function hide() {
    open = pinned;
  }

  function toggle() {
    pinned = !pinned;
    open = pinned;
  }

  function dismiss(refocus = false) {
    pinned = false;
    open = false;
    if (refocus) ringButton?.focus();
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && open) {
      event.preventDefault();
      dismiss(true);
    }
  }

  $effect(() => {
    if (!pinned) return;
    const onPointerDown = (event: PointerEvent) => {
      if (chip && event.target instanceof Node && !chip.contains(event.target)) {
        dismiss(false);
      }
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    return () => document.removeEventListener("pointerdown", onPointerDown, true);
  });
</script>

<svelte:window onkeydown={onKeydown} />

{#if runtime.mode === "native"}
  <div class="context-chip {level}" bind:this={chip}>
    <button
      class="ring-button"
      type="button"
      aria-label={label}
      aria-haspopup="dialog"
      aria-expanded={open}
      onmouseenter={show}
      onmouseleave={hide}
      onfocus={show}
      onblur={hide}
      onclick={toggle}
    >
      <svg width="16" height="16" viewBox="0 0 22 22" aria-hidden="true">
        <circle cx="11" cy="11" r={RADIUS} class="ring-track" />
        <circle
          cx="11"
          cy="11"
          r={RADIUS}
          class="ring-fill"
          style:stroke-dasharray="{CIRCUMFERENCE * fill} {CIRCUMFERENCE}"
          transform="rotate(-90 11 11)"
        />
      </svg>
      <span class="ring-pct">{hasWindow ? `${pct}%` : "–"}</span>
    </button>
    <div class="context-tip" class:open={open} role="dialog" aria-label="Lead context detail">
      <strong>Lead context</strong>
      {#if hasWindow}
        <p class="context-total">
          {formatTokenCount(runtime.usedTokens)} / {formatTokenCount(runtime.maxTokens)} tokens
          <span class="context-pct">{pct}%</span>
        </p>
        {#if runtime.contextBreakdown}
          <dl>
            {#each roles as { role, tokens, share } (role)}
              <div>
                <dt>{role}</dt>
                <span class="role-bar" aria-hidden="true"><span style:width="{share * 100}%"></span></span>
                <dd>{formatTokenCount(tokens)}</dd>
              </div>
            {/each}
          </dl>
        {:else}
          <p class="context-note">No per-role breakdown yet for this session.</p>
        {/if}
      {:else}
        <p class="context-note">The context window size is unknown for this session yet.</p>
      {/if}
      <button
        class="compact-button"
        type="button"
        disabled={compactDisabled}
        onclick={() => void actions.compactLead()}
      >
        {runtime.compacting ? "Compacting…" : "Compact now"}
      </button>
    </div>
  </div>
{/if}

<style>
  .context-chip { position: relative; display: inline-flex; }
  .ring-button { display: inline-flex; align-items: center; gap: 4px; padding: 3px 5px; border: 0; border-radius: 8px; background: transparent; cursor: pointer; font-size: var(--text-xs); font-weight: 600; }
  .ring-button:hover, .ring-button:focus-visible { background: var(--surface-soft); }
  .ring-button:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .ring-track { fill: none; stroke: var(--line); stroke-width: 3; }
  .ring-fill { fill: none; stroke-width: 3; stroke-linecap: round; transition: stroke-dasharray 240ms ease; }
  .context-chip.level-ok .ring-fill, .context-chip.level-ok .context-pct { stroke: #34d399; color: #34d399; }
  .context-chip.level-warn .ring-fill, .context-chip.level-warn .context-pct { stroke: #fbbf24; color: #fbbf24; }
  .context-chip.level-critical .ring-fill, .context-chip.level-critical .context-pct { stroke: #f87171; color: #f87171; }
  .context-chip.level-unknown .ring-fill, .context-chip.level-unknown .context-pct { stroke: var(--line-strong); color: var(--line-strong); }
  .ring-pct { color: var(--ink-soft); font-variant-numeric: tabular-nums; }
  .context-tip { position: absolute; top: calc(100% + 7px); right: 0; z-index: 30; width: 228px; padding: 10px 12px; border: 1px solid var(--line-strong); border-radius: 10px; background: var(--surface); box-shadow: var(--shadow); opacity: 0; visibility: hidden; pointer-events: none; transition: opacity 120ms ease; }
  .context-tip.open { opacity: 1; visibility: visible; pointer-events: auto; }
  .context-tip strong { display: block; margin-bottom: 7px; font-size: var(--text-xs); font-weight: 650; letter-spacing: 0.04em; text-transform: uppercase; color: var(--muted); }
  .context-total { display: flex; align-items: baseline; justify-content: space-between; gap: 10px; margin: 0 0 8px; color: var(--ink); font-family: var(--font-mono); font-size: var(--text-xs); font-variant-numeric: tabular-nums; }
  .context-pct { font-weight: 700; }
  .context-tip dl { display: grid; gap: 4px; margin: 0 0 10px; }
  .context-tip dl div { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .context-tip dt { flex: 0 0 auto; color: var(--muted); font-size: var(--text-xs); }
  .role-bar { flex: 1; position: relative; height: 3px; overflow: hidden; border-radius: 2px; background: var(--line); }
  .role-bar > span { position: absolute; inset: 0 auto 0 0; border-radius: inherit; background: var(--accent); transition: width 240ms ease; }
  .context-tip dd { flex: 0 0 auto; margin: 0; color: var(--ink); font-family: var(--font-mono); font-size: var(--text-xs); font-variant-numeric: tabular-nums; }
  .context-note { margin: 0 0 10px; color: var(--warning); font-size: var(--text-2xs); line-height: 1.45; }
  .compact-button { width: 100%; padding: 6px 8px; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface); color: var(--ink); cursor: pointer; font-size: var(--text-xs); font-weight: 600; }
  .compact-button:hover:not(:disabled) { border-color: var(--accent); color: var(--accent-strong); }
  .compact-button:disabled { opacity: 0.55; cursor: default; }
</style>