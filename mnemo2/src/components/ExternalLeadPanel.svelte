<script lang="ts">
  import { actions, app } from "../lib/store";
  import { selectNeedsYouWorkers, selectRunningWorkers } from "../lib/workerSelection";
  import { workerStatusText } from "../lib/types";

  /** Show the built-in lead chat behind this panel. */
  let { onReveal }: { onReveal: () => void } = $props();

  const running = $derived(selectRunningWorkers($app.workers));
  const needsYou = $derived(selectNeedsYouWorkers($app.workers));

  // "Leading since HH:MM" is the local clock at attach time; the host has
  // not reported one while sinceMs is null.
  const leadingSince = $derived.by(() => {
    const sinceMs = $app.externalLead.sinceMs;
    if (sinceMs === null) return null;
    const at = new Date(sinceMs);
    const hours = String(at.getHours()).padStart(2, "0");
    const minutes = String(at.getMinutes()).padStart(2, "0");
    return `${hours}:${minutes}`;
  });
</script>

<div class="external-lead-panel">
  <article class="external-notice">
    <div class="notice-label">EXTERNAL LEAD</div>
    <h2>Claude Code is leading this project</h2>
    <p class="notice-body">
      Plans, briefs, reviews and merges come from your Claude Code session. Talk to the lead there
      and follow the team's work here.
    </p>
    {#if leadingSince !== null}
      <p class="notice-meta">Leading since {leadingSince}</p>
    {/if}
    <div class="external-team" role="status" aria-live="polite">
      {#if running.length > 0}
        <p>
          <span class="team-label">Working:</span>
          {#each running as worker, index (worker.id)}
            {#if index > 0}<span class="team-sep">, </span>{/if}
            <button
              class="team-worker"
              type="button"
              aria-label={`Show ${worker.name}'s output`}
              onclick={() => actions.openWorkerPanel(worker.id)}
            >{worker.name}</button>
          {/each}
        </p>
      {:else}
        <p>No workers running right now.</p>
      {/if}
      {#if needsYou.length > 0}
        <p>
          <span class="team-label">Needs you:</span>
          {#each needsYou as worker, index (worker.id)}
            {#if index > 0}<span class="team-sep">, </span>{/if}
            <button
              class="team-worker"
              type="button"
              aria-label={`Open ${worker.name}`}
              onclick={() => actions.openWorkerPanel(worker.id)}
            >{worker.name}</button>
            <span class="team-status"> ({workerStatusText(worker)})</span>
          {/each}
        </p>
      {/if}
    </div>
    <div class="notice-actions">
      <button class="button button-quiet" type="button" onclick={onReveal}>Open the built-in lead anyway</button>
    </div>
  </article>
</div>

<style>
  .external-lead-panel { display: flex; flex: 1; min-height: 0; flex-direction: column; justify-content: center; overflow-y: auto; padding: 22px 32px; }
  .external-notice { max-width: 62ch; margin: 0; padding: 20px 22px 18px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
  .notice-label { margin-bottom: 6px; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.08em; }
  h2 { margin: 0 0 8px; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 550; }
  .notice-body { margin: 0; color: var(--ink-soft); font-size: var(--text-body); line-height: 1.55; }
  .notice-meta { margin: 7px 0 0; color: var(--muted); font-size: var(--text-body); }
  .external-team { margin-top: 14px; padding-top: 10px; border-top: 1px solid var(--line); }
  .external-team p { display: flex; flex-wrap: wrap; align-items: baseline; margin: 0 0 5px; color: var(--muted); font-size: var(--text-body); }
  .external-team p:last-child { margin-bottom: 0; }
  .team-label { margin-right: 4px; font-weight: 650; }
  .team-sep { color: var(--muted); }
  .team-status { color: var(--ink-soft); }
  .team-worker { padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font: inherit; font-weight: 650; }
  .team-worker:hover { text-decoration: underline; }
  .notice-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 14px; }
  @media (max-width: 760px) { .external-lead-panel { padding: 16px; } }
</style>