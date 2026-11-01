<script lang="ts">
  import {
    CheckCircle2,
    GitBranch,
    GitMerge,
    LoaderCircle,
    Pause,
    Play,
    Square,
    Trash2,
    XCircle,
  } from "@lucide/svelte";
  import { workerPlanLabel } from "../lib/boardState";
  import { actions, app } from "../lib/store";
  import { canMerge, isAuditWorker } from "../lib/review";
  import { workerStatusText, type Worker } from "../lib/types";
  import { deriveTaskWorkflow } from "../lib/workflow";
  import Collapsible from "./Collapsible.svelte";
  import FindingsPanel from "./FindingsPanel.svelte";
  import Markdown from "./Markdown.svelte";
  import ModelBadge from "./ModelBadge.svelte";
  import ModelEffortPicker from "./ModelEffortPicker.svelte";
  import ReviewEvidence from "./ReviewEvidence.svelte";
  import TaskWorkflow from "./TaskWorkflow.svelte";
  import WorkerInbox from "./WorkerInbox.svelte";
  import WorkerThread from "./WorkerThread.svelte";

  let { worker }: { worker: Worker } = $props();
  let tab = $state<"review" | "thread" | "timeline">("thread");
  let lastWorkerId = "";

  const openFindings = $derived(worker.findings.filter((finding) => finding.status === "open").length);
  const threadMessages = $derived(worker.messages.filter((entry) => entry.kind !== "tool").length);
  const planLabel = $derived(workerPlanLabel($app.board, worker));
  const workflow = $derived(deriveTaskWorkflow(worker));
  const mergeEligibility = $derived(canMerge(worker));
  const audit = $derived(isAuditWorker(worker));
  const highRisk = $derived(worker.reviewRisk === "high");
  const models = $derived($app.leadRuntime.models);
  /** The model the worker was given: its spec, else the configured default. */
  const modelSpec = $derived(
    worker.modelSpec ?? models.find((model) => model.is_default)?.spec ?? "",
  );
  /** A worker can move to another model unless it is mid-turn or its task is over. */
  const modelChangeable = $derived(
    models.some((model) => model.spec === modelSpec) &&
      !["working", "dispatching", "merging", "merged", "cancelled"].includes(worker.status),
  );

  $effect(() => {
    const id = worker.id;
    if (id === lastWorkerId) return;
    lastWorkerId = id;
    tab = "thread";
  });

</script>

<article class="worker-detail">
  <header class="worker-hero">
    <div class="worker-identity">
      <span class="avatar" style="--hue: {worker.hue}">{worker.initials}</span>
      <div class="worker-heading">
        <div class="worker-title">
          <h2>{worker.name}<span data-status={worker.status}>{workerStatusText(worker)}</span></h2>
          {#if audit}
            <span class="kind-badge" data-kind="audit">Audit</span>
          {/if}
          {#if highRisk}
            <span class="kind-badge" data-risk="high">High risk</span>
          {/if}
          {#if modelChangeable}
            <ModelEffortPicker
              {models}
              spec={modelSpec}
              effort={worker.reasoningEffort ?? null}
              label={worker.name}
              compact
              onchange={(spec, effort) => actions.setWorkerModel(worker.id, spec, effort)}
            />
          {:else}
            <span class="model-locked" title="Pause or stop the worker to change its model"><ModelBadge model={worker.model} /></span>
            {#if worker.reasoningEffort}<span class="effort-pill" title="Reasoning effort">effort: {worker.reasoningEffort}</span>{/if}
          {/if}
          {#if worker.tier}<span class="effort-pill" title="Model tier">tier: {worker.tier}</span>{/if}
        </div>
        {#if planLabel}<p class="worker-plan">{planLabel}</p>{/if}
        <!-- Three lines of Markdown prose: 3 × (14px font × 1.62 line-height) ≈ 68px. -->
        <div class="worker-brief">
          <Collapsible maxHeight={68} label="worker brief">
            <Markdown source={worker.task} onOpenLink={actions.openBrowser} />
          </Collapsible>
        </div>
        {#if audit}
          <p class="audit-scope">
            Read-only audit{#if worker.auditTarget} of <code>{worker.auditTarget}</code>{/if}{#if worker.auditTip} at <code>{worker.auditTip}</code>{/if} · no plan step, no merge
          </p>
        {:else}
          <TaskWorkflow {workflow} />
        {/if}
      </div>
    </div>
    <div class="worker-actions">
      <button class="button button-quiet" onclick={() => actions.openWorkerDiff(worker.id)}>Diff</button>
      <button class="button button-quiet" disabled={worker.status === "cancelled"} onclick={() => actions.steerWorker(worker.id)}>Steer</button>
      <button class="button button-quiet" disabled={worker.compacting || !["completed", "idle", "needs_you", "paused", "stopped", "interrupted", "failed"].includes(worker.status)} onclick={() => actions.compactWorker(worker.id)}>Compact</button>
      {#if worker.status === "working" || worker.status === "dispatching"}
        <button class="button button-quiet" onclick={() => actions.pauseWorker(worker.id)}><Pause size={11} /> Pause</button>
      {:else if worker.status === "paused" || worker.status === "stopped" || worker.status === "interrupted"}
        <button class="button button-quiet" onclick={() => actions.resumeWorker(worker.id)}><Play size={11} /> Resume</button>
      {/if}
      <button class="button button-quiet" disabled={["stopped", "merging", "merged", "cancelled"].includes(worker.status)} onclick={() => actions.stopWorker(worker.id)}><Square size={11} /> Stop</button>
      <button class="button button-quiet" aria-label={`Delete ${worker.name}`} title="Delete this worker and its history" disabled={worker.status === "merging"} onclick={() => actions.requestDeleteWorkers([worker.id])}><Trash2 size={11} /> Delete</button>
    </div>
  </header>

  {#if worker.needsYou}
    <p class="worker-blocker" role="status">
      <strong>Needs you:</strong>
      {worker.summary || "the worker stopped before finishing — read its thread for its last message, then reply, resume, or stop it."}
    </p>
  {/if}

  <div class="file-areas"><GitBranch size={12} /><code>{worker.branch}</code><span>{worker.files.length} files</span></div>

  <nav class="detail-tabs" aria-label="Worker detail sections">
    <button class:active={tab === "thread"} aria-pressed={tab === "thread"} onclick={() => { tab = "thread"; }}>Thread <span>{threadMessages}</span></button>
    <button class:active={tab === "review"} aria-pressed={tab === "review"} onclick={() => { tab = "review"; }}>Review {#if openFindings > 0}<span>{openFindings}</span>{/if}</button>
    <button class:active={tab === "timeline"} aria-pressed={tab === "timeline"} onclick={() => { tab = "timeline"; }}>Timeline</button>
  </nav>

  <div class="detail-body">
    {#if tab === "review"}
      <section class="checks-panel">
        <div class="checks-heading"><span>BRANCH-TIP CHECKS</span><code>{worker.worktree}</code></div>
        <div class="usage-line">{worker.tokens} cumulative tokens · {worker.turns} completed turns · {worker.elapsed || "—"}</div>
        {#each worker.checks as check (check.name)}
          <div class="check-row">
            {#if check.status === "passed"}<CheckCircle2 size={15} />{:else if check.status === "running"}<LoaderCircle size={15} />{:else}<XCircle size={15} />{/if}
            <div><strong>{check.name}</strong><small>{check.detail}</small></div>
            <span data-status={check.status}>{check.status}</span>
          </div>
        {/each}
      </section>
      {#if !audit}
        <ReviewEvidence {worker} />
      {/if}
      <FindingsPanel {worker} />

      <section class="merge-panel">
        <div class="merge-decision">
          {#if audit}
            <div>
              <p>READ-ONLY AUDIT</p>
              <h3>Audits never merge</h3>
              <span>
                This worker inspects another worker's tip and reports to the lead; it holds no plan
                step and no merge action.
              </span>
            </div>
          {:else if worker.merge}
            <div>
              <p>MERGE RECORD · {worker.merge.target}</p>
              <h3><code>{worker.merge.commit}</code></h3>
              <span>{worker.merge.files} files · +{worker.merge.additions} −{worker.merge.deletions} · {worker.merge.reviewSummary}</span>
            </div>
            {#if worker.merge.revertCommit}
              <span class="merge-reverted">Reverted · <code>{worker.merge.revertCommit}</code></span>
            {:else}
              <button class="button button-quiet" aria-label="Revert merged branch" onclick={() => actions.revertMerge(worker.id)}>Revert</button>
            {/if}
          {:else}
            <div>
              <p>TEAMMANAGER · SERIALIZED MERGE</p>
              <h3>Merge into the lead branch</h3>
              {#if mergeEligibility.ok}
                <span>{worker.review?.summary ?? "Review closed"} · checks passed on the reviewed tip.</span>
              {:else}
                <span>{mergeEligibility.reason}</span>
              {/if}
            </div>
            <button class="button button-primary" aria-label="Merge reviewed branch" disabled={!mergeEligibility.ok || worker.status === "merging"} onclick={() => actions.mergeWorker(worker.id)}><GitMerge size={14} /> Merge reviewed branch</button>
          {/if}
        </div>
      </section>
    {:else if tab === "thread"}
      <WorkerInbox {worker} />
      <WorkerThread {worker} />
    {:else}
      <section class="timeline-panel" aria-label={`${worker.name} timeline`}>
        {#each worker.timeline as step (step.id)}
          <div class="timeline-step" data-status={step.status}>
            <span class="timeline-marker"><span></span></span>
            <div><div><strong>{step.label}</strong>{#if step.time}<time>{step.time}</time>{/if}</div><p>{step.detail}</p><span>{step.status}</span></div>
          </div>
        {/each}
      </section>
    {/if}
  </div>
</article>

<style>
  .worker-detail { height: 100%; overflow-y: auto; padding: 22px; }
  .worker-hero { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
  .worker-identity { display: flex; gap: 12px; min-width: 0; }
  .avatar { display: grid; flex: 0 0 auto; width: 44px; height: 44px; place-items: center; border-radius: 13px; background: hsl(var(--hue) 32% 87%); color: hsl(var(--hue) 30% 32%); font-size: 12px; font-weight: 750; }
  .worker-heading { min-width: 0; }
  .worker-plan { margin: 2px 0 0; color: var(--accent-strong); font-size: 9px; }
  .worker-brief { margin: 10px 0 0; }
  .worker-title { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .worker-identity h2 { display: flex; align-items: center; gap: 8px; margin: 0; font-family: var(--font-prose); font-size: 26px; font-weight: 550; }
  .worker-identity h2 span { padding: 4px 7px; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-ui); font-size: 7px; font-weight: 650; }
  .worker-identity h2 span[data-status="in_review"], .worker-identity h2 span[data-status="conflict"] { background: var(--warning-soft); color: var(--warning); }
  .kind-badge { padding: 3px 7px; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-ui); font-size: 7px; font-weight: 650; letter-spacing: 0.05em; text-transform: uppercase; }
  .kind-badge[data-kind="audit"] { background: var(--accent-soft); color: var(--accent-strong); }
  .kind-badge[data-risk="high"] { background: #f4e4e1; color: var(--danger); }
  .kind-badge { padding: 3px 7px; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-ui); font-size: 7px; font-weight: 650; letter-spacing: 0.05em; text-transform: uppercase; }
  .kind-badge[data-kind="audit"] { background: var(--accent-soft); color: var(--accent-strong); }
  .kind-badge[data-risk="high"] { background: #f4e4e1; color: var(--danger); }
  .audit-scope { margin: 6px 0 0; color: var(--muted); font-size: 9px; line-height: 1.5; }
  .audit-scope code { font-family: var(--font-mono); font-size: 9px; }
  .effort-pill { padding: 3px 6px; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); font-size: 8px; font-weight: 600; }
  .checks-heading code { overflow: hidden; font-family: var(--font-mono); font-size: 7px; letter-spacing: 0; text-overflow: ellipsis; white-space: nowrap; }
  .usage-line { margin: -3px 0 10px; color: var(--muted); font-size: 8px; }
  .check-row { display: flex; align-items: center; gap: 9px; }
  .check-row > :global(svg) { color: var(--accent); }
  .check-row div { flex: 1; }
  .check-row strong, .check-row small { display: block; }
  .check-row strong { font-size: 10px; font-weight: 600; }

  .merge-decision { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
  .merge-decision p { margin: 0 0 4px; color: var(--muted); font-size: 7px; letter-spacing: 0.09em; }
  .merge-decision h3 { margin: 0 0 4px; font-size: 12px; font-weight: 650; }
  .merge-decision div > span { display: block; max-width: 360px; color: var(--muted); font-size: 8px; line-height: 1.45; }
  .merge-decision button { flex: 0 0 auto; }
  .timeline-step { display: grid; grid-template-columns: 20px 1fr; gap: 8px; min-height: 76px; }
  .timeline-marker { display: flex; justify-content: center; border-left: 1px solid var(--line); margin-left: 6px; }
  .timeline-marker > span { width: 8px; height: 8px; margin: 5px 0 0 -4.5px; border-radius: 50%; background: var(--line-strong); }
  .timeline-step[data-status="done"] .timeline-marker > span { background: var(--accent); }
  .timeline-step[data-status="active"] .timeline-marker > span { background: var(--accent); box-shadow: 0 0 0 4px var(--accent-soft); }
  .timeline-step strong { font-size: 11px; }
  .timeline-step time { margin-left: 8px; color: var(--muted); font-size: 8px; }
  .timeline-step p { margin: 4px 0; color: var(--muted); font-size: 9px; }
  .timeline-step div > span { color: var(--muted); font-size: 7px; text-transform: uppercase; }
  @media (max-width: 620px) { .worker-detail { padding: 16px; } .worker-hero { flex-direction: column; } }
</style>
