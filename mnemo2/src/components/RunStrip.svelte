<script lang="ts">
  import { Activity, ChevronDown } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
</script>

<section class="run-strip" aria-label="Lead activity">
  <button class="run-summary" onclick={actions.toggleActivity} aria-expanded={$app.activityExpanded}>
    <span class="run-title"><Activity size={14} /> Run 04</span>
    <span class="run-pills" aria-hidden="true">
      {#each $app.leadTimeline as step, index (step.id)}
        {#if index > 0}<span class="connector"></span>{/if}
        <span class="run-pill" data-status={step.status}>
          <span class="step-dot"></span>{step.label}
        </span>
      {/each}
    </span>
    <span class="run-state">Review in progress <ChevronDown size={14} /></span>
  </button>

  {#if $app.activityExpanded}
    <div class="run-details">
      {#each $app.leadTimeline as step (step.id)}
        <div class="run-detail" data-status={step.status}>
          <span class="detail-marker"><span></span></span>
          <div>
            <div class="detail-heading"><strong>{step.label}</strong>{#if step.time}<time>{step.time}</time>{/if}</div>
            <p>{step.detail}</p>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .run-strip { margin: 0 auto; width: min(100%, 820px); }
  .run-summary {
    display: flex;
    align-items: center;
    gap: 16px;
    width: 100%;
    min-height: 46px;
    padding: 8px 12px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: rgb(255 255 255 / 72%);
    color: var(--muted);
    cursor: pointer;
    text-align: left;
  }
  .run-summary:hover { border-color: var(--line-strong); background: var(--surface); }
  .run-title { display: inline-flex; align-items: center; gap: 6px; color: var(--ink-soft); font-size: 11px; font-weight: 650; white-space: nowrap; }
  .run-pills { display: flex; flex: 1; align-items: center; min-width: 0; }
  .run-pill { display: inline-flex; align-items: center; gap: 5px; color: var(--muted); font-size: 10px; white-space: nowrap; }
  .run-pill[data-status="active"] { color: var(--accent-strong); font-weight: 650; }
  .step-dot { width: 6px; height: 6px; border: 1px solid var(--line-strong); border-radius: 50%; background: var(--surface); }
  .run-pill[data-status="done"] .step-dot { border-color: var(--accent); background: var(--accent); }
  .run-pill[data-status="active"] .step-dot { border-color: var(--accent); background: var(--accent-soft); box-shadow: 0 0 0 3px var(--accent-soft); }
  .connector { flex: 1; min-width: 10px; height: 1px; margin: 0 7px; background: var(--line); }
  .run-state { display: inline-flex; align-items: center; gap: 5px; font-size: 10px; white-space: nowrap; }
  .run-details { padding: 14px 14px 6px; border: 1px solid var(--line); border-top: 0; border-radius: 0 0 12px 12px; background: var(--surface); }
  .run-detail { display: grid; grid-template-columns: 18px 1fr; gap: 8px; min-height: 48px; }
  .detail-marker { display: flex; justify-content: center; border-left: 1px solid var(--line); margin-left: 5px; }
  .detail-marker span { width: 7px; height: 7px; margin: 6px 0 0 -4px; border-radius: 50%; background: var(--line-strong); }
  .run-detail[data-status="done"] .detail-marker span { background: var(--accent); }
  .run-detail[data-status="active"] .detail-marker span { background: var(--accent); box-shadow: 0 0 0 4px var(--accent-soft); }
  .detail-heading { display: flex; gap: 8px; align-items: baseline; font-size: 12px; }
  .detail-heading time { color: var(--muted); font-size: 10px; }
  .run-detail p { margin: 3px 0 12px; color: var(--muted); font-size: 11px; }
  @media (max-width: 760px) {
    .run-pills { display: none; }
    .run-summary { justify-content: space-between; }
  }
</style>
