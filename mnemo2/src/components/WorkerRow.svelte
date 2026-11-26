<script lang="ts">
  import { AlertCircle, ChevronRight } from "@lucide/svelte";
  import { workerPlanLabel } from "../lib/boardState";
  import { briefSummary } from "../lib/brief";
  import { app } from "../lib/store";
  import { workerPhaseLabel } from "../lib/workerActivity";
  import { hostAway, hostBadgeTitle } from "../lib/workerHost";
  import { workerAgentPhaseText, workerStatusText, type Worker } from "../lib/types";
  import { nudgeChipText, routingChipText } from "../lib/workerRouting";
  import ActivityStatus from "./ActivityStatus.svelte";
  import ElapsedTime from "./ElapsedTime.svelte";
  import ModelBadge from "./ModelBadge.svelte";

  let {
    worker,
    selected = false,
    compact = false,
    onSelect,
  }: {
    worker: Worker;
    selected?: boolean;
    compact?: boolean;
    onSelect: (workerId: string) => void;
  } = $props();

  const planLabel = $derived(workerPlanLabel($app.board, worker));
  /** One plain paragraph of the brief; the row clamps it to three lines. */
  const taskSummary = $derived(briefSummary(worker.task));
  /** The dispatch decision chip, when the dispatch recorded one. */
  const routingChip = $derived(routingChipText(worker.routing));
  /** The live nudge chip: it clears on a tier change or when the run ends. */
  const nudgeChip = $derived(nudgeChipText(worker.routing, worker.tier, worker.status));
  /** The Claude Code agent phase, while the worker waits on or runs one. */
  const agentPhaseText = $derived(workerAgentPhaseText(worker.agentPhase));
  /** The other computer this worker runs on; null when it runs on this one. */
  const host = $derived(worker.host ?? null);
  /** Seconds its host has been away, live only while the worker is active (D6). */
  const awaySeconds = $derived(hostAway(worker, Date.now()));
</script>

<button
  class="worker-row"
  class:selected
  class:compact
  data-status={worker.status}
  aria-label={`Open ${worker.name} worker thread`}
  aria-current={selected ? "true" : undefined}
  onclick={() => onSelect(worker.id)}
>
  <span class="avatar" style="--hue: {worker.hue}">{worker.initials}</span>
  <span class="worker-copy">
    <span class="worker-name"><strong>{worker.name}</strong>{#if worker.needsYou}<AlertCircle size={11} />{/if}{#if worker.kind === "audit"}<span class="row-badge" data-kind="audit">Audit</span>{/if}{#if worker.reviewRisk === "high"}<span class="row-badge" data-risk="high">High risk</span>{/if}{#if host}<span class="row-badge host-badge" class:away={awaySeconds !== null} title={hostBadgeTitle(worker)}>{host.name}{#if awaySeconds !== null && host.offlineSinceMs !== null}{" · away "}<ElapsedTime startedAtMs={host.offlineSinceMs} prefix="" title={hostBadgeTitle(worker)} />{/if}</span>{/if}<ModelBadge model={worker.model} /></span>
    {#if routingChip || nudgeChip}
      <span class="worker-route">{#if routingChip}<span class="route-chip" data-kind="routing">{routingChip}</span>{/if}{#if nudgeChip}<span class="route-chip" data-kind="nudge">{nudgeChip}</span>{/if}</span>
    {/if}
    {#if planLabel}<span class="worker-plan">{planLabel}</span>{/if}
    <span class="worker-task" title={taskSummary}>{taskSummary}</span>
    {#if !compact}<span class="worker-progress"><span style={`width: ${worker.progress}%`}></span></span>{/if}
  </span>
  <span class="worker-meta">
    <span class="status-label" data-status={worker.status} data-agent-phase={worker.agentPhase ?? undefined} title={agentPhaseText ?? undefined}>{agentPhaseText ?? workerStatusText(worker)}</span>
    {#if worker.activity}
      <span class="worker-live">
        <ActivityStatus
          label={workerPhaseLabel(worker.activity)}
          startedAtMs={worker.createdAtMs}
          pulse={worker.activity.phase !== "waiting"}
          size="meta"
          truncate
        />
      </span>
    {:else}
      <small>{#if worker.cost != null}${worker.cost.toFixed(2)}{:else}{worker.tokens} tokens{/if} · {worker.elapsed || "—"}</small>
    {/if}
  </span>
  <ChevronRight class="row-chevron" size={13} />
</button>

<style>
  .worker-row { display: flex; align-items: center; gap: 10px; width: 100%; min-height: 66px; padding: 10px 11px; border: 1px solid transparent; border-radius: 11px; background: transparent; color: var(--ink); cursor: pointer; text-align: left; }
  .worker-row:hover { background: var(--surface-soft); }
  .worker-row.selected { border-color: var(--accent-line); background: var(--accent-soft); }
  .worker-row.compact { min-height: 56px; padding: 8px 7px; }
  .avatar { display: grid; flex: 0 0 auto; width: 32px; height: 32px; place-items: center; border-radius: 10px; background: hsl(var(--hue) 32% var(--avatar-bg-lightness)); color: hsl(var(--hue) 30% var(--avatar-ink-lightness)); font-size: var(--text-meta); font-weight: 750; }
  .worker-copy { flex: 1; min-width: 0; }
  /* One status column, the same x and the same width on every row: the pill keeps
     its natural width and the line under it fills the column, truncating its label. */
  .worker-meta { display: flex; flex: 0 0 152px; flex-direction: column; align-items: flex-start; min-width: 0; }
  .worker-name { display: flex; align-items: center; gap: 5px; }
  .worker-name strong { font-size: var(--text-title); font-weight: 650; }
  .worker-name :global(svg) { color: var(--warning); }
  .worker-name :global(.model-badge) { flex: 0 1 auto; max-width: 130px; margin-left: auto; overflow: hidden; text-overflow: ellipsis; }
  .row-badge { flex: 0 0 auto; padding: 2px 5px; border-radius: 5px; background: var(--surface-soft); color: var(--ink-soft); font-size: var(--text-meta); font-weight: 650; }
  .row-badge[data-kind="audit"] { background: var(--accent-soft); color: var(--accent-strong); }
  .row-badge[data-risk="high"] { background: var(--risk-bg); color: var(--danger); }
  /* The host badge names the computer, and turns into a warning while its link
     is away and the worker is still active on it. */
  .host-badge { white-space: nowrap; }
  .host-badge.away { background: var(--warning-soft); color: var(--warning); }
  .host-badge.away :global(.elapsed-time) { font-variant-numeric: tabular-nums; }
  .worker-plan { display: block; margin-top: 2px; overflow: hidden; color: var(--accent-strong); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .worker-task { display: -webkit-box; line-clamp: 3; -webkit-line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; color: var(--muted); font-family: var(--font-ui); font-size: var(--text-meta); }
  .worker-progress { display: block; height: 2px; margin-top: 7px; overflow: hidden; border-radius: 2px; background: var(--line); }
  .worker-progress > span { display: block; height: 100%; border-radius: inherit; background: var(--accent); transition: width 240ms ease; }
  .worker-meta small { display: block; width: 100%; min-width: 0; margin-top: 4px; overflow: hidden; color: var(--muted); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .worker-live { display: block; width: 100%; min-width: 0; margin-top: 4px; white-space: nowrap; }
  .status-label { display: inline-flex; padding: 3px 6px; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); font-size: var(--text-meta); font-weight: 650; white-space: nowrap; }
  .status-label[data-agent-phase] { max-width: 100%; overflow: hidden; text-overflow: ellipsis; }
  .status-label[data-status="in_review"], .status-label[data-status="conflict"] { background: var(--warning-soft); color: var(--warning); }
  .status-label[data-status="failed"] { background: var(--risk-bg); color: var(--danger); }
  .status-label[data-status="merged"] { background: var(--accent-soft); color: var(--accent-strong); }
  .status-label[data-status="working"], .status-label[data-status="fixing"] { background: var(--running-bg); color: var(--running-ink); }
  .worker-row :global(.row-chevron) { flex: 0 0 auto; color: var(--line-strong); }
  .route-chip { flex: 0 0 auto; padding: 2px 5px; border-radius: 5px; background: var(--surface-soft); color: var(--ink-soft); font-size: var(--text-meta); font-weight: 650; white-space: nowrap; }
  .route-chip[data-kind="nudge"] { background: var(--warning-soft); color: var(--warning); }
  /* The routing and nudge chips keep a small line of their own under the name, so
     the model badge on the name line never has to shrink away behind them. */
  .worker-route { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 3px; }
  .worker-row.compact .worker-meta small, .worker-row.compact .worker-live { display: none; }
  @media (max-width: 1100px) { .worker-meta small, .worker-live { display: none; } }
</style>
