<script lang="ts">
  import { tick } from "svelte";
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
  import { workerStatusText, type TaskWorkflowStepId, type Worker } from "../lib/types";
  import { deriveTaskWorkflow } from "../lib/workflow";
  import FindingsPanel from "./FindingsPanel.svelte";
  import Markdown from "./Markdown.svelte";
  import ModelBadge from "./ModelBadge.svelte";
  import ModelEffortPicker from "./ModelEffortPicker.svelte";
  import ReviewEvidence from "./ReviewEvidence.svelte";
  import TaskWorkflow from "./TaskWorkflow.svelte";
  import WorkerInbox from "./WorkerInbox.svelte";
  import WorkerThread from "./WorkerThread.svelte";

  const BRIEF_MAX_HEIGHT = 42;

  let {
    worker,
    measureBriefOverflow = defaultMeasureBriefOverflow,
  }: {
    worker: Worker;
    /** Layout measurement seam for non-browser tests. */
    measureBriefOverflow?: (element: HTMLElement) => boolean;
  } = $props();

  let tab = $state<"review" | "thread" | "timeline">("thread");
  let lastWorkerId = "";
  let briefContent = $state<HTMLDivElement>();
  let briefExpanded = $state(false);
  let briefOverflowing = $state(false);
  let mergePanel = $state<HTMLElement>();

  const openFindings = $derived(worker.findings.filter((finding) => finding.status === "open").length);
  const threadMessages = $derived(worker.messages.filter((entry) => entry.kind !== "tool").length);
  const planLabel = $derived(workerPlanLabel($app.board, worker));
  const workflow = $derived(deriveTaskWorkflow(worker));
  const mergeEligibility = $derived(canMerge(worker));
  const audit = $derived(isAuditWorker(worker));
  /** An audit that delivered its report is over: it cannot be steered,
   * compacted, resumed or messaged — the lead adjudicates the report. */
  const auditFinished = $derived(
    audit && (worker.reportDelivered === true || worker.status === "completed"),
  );
  const highRisk = $derived(worker.reviewRisk === "high");
  const models = $derived($app.leadRuntime.models);
  /** The model the worker was given: its spec, else the configured default. */
  const modelSpec = $derived(
    worker.modelSpec ?? models.find((model) => model.is_default)?.spec ?? "",
  );
  /**
   * A worker can move to another model unless its task is over or it only
   * reviews (an audit's model is never user-switchable). A worker mid-run
   * switches live: the current call finishes, the next one uses the new model.
   */
  const modelChangeable = $derived(
    !audit &&
      models.some((model) => model.spec === modelSpec) &&
      !["merging", "merged", "cancelled"].includes(worker.status),
  );

  $effect(() => {
    const id = worker.id;
    if (id === lastWorkerId) return;
    lastWorkerId = id;
    tab = "thread";
    briefExpanded = false;
  });

  $effect(() => {
    const element = briefContent;
    const source = worker.task;
    const measure = measureBriefOverflow;
    if (!element || !source) {
      briefOverflowing = false;
      return;
    }
    const update = () => { briefOverflowing = measure(element); };
    update();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(update);
    observer.observe(element);
    const markdown = element.querySelector(".markdown");
    if (markdown) observer.observe(markdown);
    return () => observer.disconnect();
  });

  /** Compare the Markdown's full height with its visible clamped height. */
  function defaultMeasureBriefOverflow(element: HTMLElement): boolean {
    const visibleHeight = Math.min(element.clientHeight, BRIEF_MAX_HEIGHT);
    return element.scrollHeight > visibleHeight + 1;
  }

  /** Route a workflow step to the relevant part of the worker detail. */
  function selectWorkflowStep(stepId: TaskWorkflowStepId): void {
    if (stepId === "brief") {
      briefExpanded = true;
      return;
    }
    if (stepId === "implement") {
      tab = "thread";
      return;
    }
    tab = "review";
    if (stepId === "merge") {
      void tick().then(() => mergePanel?.scrollIntoView?.({ block: "nearest" }));
    }
  }
</script>

<article class="worker-detail" aria-label={`Worker detail: ${worker.name}`}>
  <header class="worker-hero">
    <div class="worker-primary">
      <div class="worker-identity">
        <span class="avatar" style="--hue: {worker.hue}">{worker.initials}</span>
        <div class="worker-heading">
          <div class="worker-title">
            <h2>{worker.name}</h2>
            <span class="status-pill" data-status={worker.status}>
              {#if worker.status === "working" || worker.status === "dispatching"}<LoaderCircle size={9} class="spin" />{/if}
              {workerStatusText(worker)}
            </span>
            {#if audit}<span class="kind-badge" data-kind="audit">Audit</span>{/if}
            {#if highRisk}<span class="kind-badge" data-risk="high">High risk</span>{/if}
            <span class="model-control">
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
                <span
                  class="model-locked"
                  title={audit ? "An audit worker's model is not user-switchable" : "This worker's task is over, so its model cannot change"}
                ><ModelBadge model={worker.model} /></span>
              {/if}
            </span>
            {#if !modelChangeable && worker.reasoningEffort}<span class="effort-pill" title="Reasoning effort">effort: {worker.reasoningEffort}</span>{/if}
            {#if worker.tier}<span class="effort-pill" title="Model tier">tier: {worker.tier}</span>{/if}
          </div>
          {#if planLabel}<p class="worker-plan">{planLabel}</p>{/if}
        </div>
      </div>

      <div class="worker-actions" role="toolbar" aria-label="Worker actions">
        <button class="toolbar-button" type="button" aria-label="Diff" title="Open worker diff" onclick={() => actions.openWorkerDiff(worker.id)}>Diff</button>
        <button
          class="toolbar-button"
          type="button"
          aria-label={`Model calls ${worker.name}`}
          title="Every model call this worker made, in the Trace page"
          onclick={() => actions.openTrace({ agent: worker.id })}
        >Model calls</button>
        {#if !auditFinished}<button class="toolbar-button" type="button" aria-label="Steer" title="Steer worker" disabled={worker.status === "cancelled"} onclick={() => actions.steerWorker(worker.id)}>Steer</button>{/if}
        {#if !auditFinished}<button class="toolbar-button" type="button" aria-label="Compact" title="Compact worker context" disabled={worker.compacting || !["completed", "idle", "needs_you", "paused", "stopped", "interrupted", "failed"].includes(worker.status)} onclick={() => actions.compactWorker(worker.id)}>Compact</button>{/if}
        {#if worker.status === "working" || worker.status === "dispatching"}
          <button class="toolbar-button" type="button" aria-label="Pause" title="Pause worker" onclick={() => actions.pauseWorker(worker.id)}><Pause size={10} /> Pause</button>
        {:else if !auditFinished && (worker.status === "paused" || worker.status === "stopped" || worker.status === "interrupted" || (audit && worker.status === "failed"))}
          <button class="toolbar-button" type="button" aria-label="Resume" title="Resume worker" onclick={() => actions.resumeWorker(worker.id)}><Play size={10} /> Resume</button>
        {/if}
        <button class="toolbar-button" type="button" aria-label="Stop" title="Stop worker" disabled={["stopped", "merging", "merged", "cancelled"].includes(worker.status)} onclick={() => actions.stopWorker(worker.id)}><Square size={10} /> Stop</button>
        <button class="toolbar-button danger" type="button" aria-label={`Delete ${worker.name}`} title="Delete this worker and its history" disabled={worker.status === "merging"} onclick={() => actions.requestDeleteWorkers([worker.id])}><Trash2 size={10} /> Delete</button>
      </div>
    </div>

    <section class="worker-brief" aria-label="Worker brief">
      {#if briefOverflowing}
        <div class="brief-controls">
          <button
            class="brief-toggle"
            type="button"
            aria-controls={`worker-${worker.id}-brief`}
            aria-expanded={briefExpanded}
            aria-label={briefExpanded ? "Show less brief" : "Show more brief"}
            onclick={() => { briefExpanded = !briefExpanded; }}
          >{briefExpanded ? "Show less" : "Show more"}<span class="sr-only"> brief</span></button>
        </div>
      {/if}
      <div
        id={`worker-${worker.id}-brief`}
        class="brief-content"
        class:clamped={!briefExpanded && briefOverflowing}
        style:max-height={briefExpanded ? "40vh" : `${BRIEF_MAX_HEIGHT}px`}
        style:overflow-y={briefExpanded ? "auto" : "hidden"}
        bind:this={briefContent}
      >
        <Markdown source={worker.task} onOpenLink={actions.openBrowser} onOpenPath={actions.openRepoPath} />
      </div>
    </section>

    <div class="worker-meta">
      {#if audit}
        <p class="audit-scope">
          Read-only audit{#if worker.auditTarget} of <code>{worker.auditTarget}</code>{/if}{#if worker.auditTip} at <code>{worker.auditTip}</code>{/if} · no plan step, no merge
        </p>
        {#if auditFinished}
          <p class="audit-scope">Audit finished; its report is with the lead.</p>
        {/if}
      {:else}
        <TaskWorkflow {workflow} onselect={selectWorkflowStep} />
      {/if}
      <div class="file-areas"><GitBranch size={11} /><code>{worker.branch}</code><span>{worker.files.length} files</span></div>
    </div>

    <nav class="detail-tabs" aria-label="Worker detail sections">
      <button class:active={tab === "thread"} type="button" aria-pressed={tab === "thread"} onclick={() => { tab = "thread"; }}>Thread <span>{threadMessages}</span></button>
      <button class:active={tab === "review"} type="button" aria-pressed={tab === "review"} onclick={() => { tab = "review"; }}>Review {#if openFindings > 0}<span>{openFindings}</span>{/if}</button>
      <button class:active={tab === "timeline"} type="button" aria-pressed={tab === "timeline"} onclick={() => { tab = "timeline"; }}>Timeline</button>
    </nav>
  </header>

  {#if worker.needsYou}
    <p class="worker-blocker" role="status">
      <strong>Needs you:</strong>
      {worker.summary || "the worker stopped before finishing — read its thread for its last message, then reply, resume, or stop it."}
    </p>
  {/if}

  <div class="detail-body">
    {#if tab === "review"}
      <section class="checks-panel">
        <div class="checks-heading"><span>BRANCH-TIP CHECKS</span><code>{worker.worktree}</code></div>
        <div class="usage-line">{worker.tokens} cumulative tokens · {worker.turns} completed turns · {worker.elapsed || "—"}</div>
        {#each worker.checks as check (check.name)}
          <div class="check-row">
            {#if check.status === "passed"}<CheckCircle2 size={13} />{:else if check.status === "running"}<LoaderCircle size={13} />{:else}<XCircle size={13} />{/if}
            <div><strong>{check.name}</strong><small>{check.detail}</small></div>
            <span data-status={check.status}>{check.status}</span>
          </div>
        {/each}
      </section>
      {#if !audit}<ReviewEvidence {worker} />{/if}
      <FindingsPanel {worker} />

      <section class="merge-panel" bind:this={mergePanel}>
        <div class="merge-decision">
          {#if audit}
            <div>
              <p>READ-ONLY AUDIT</p>
              <h3>Audits never merge</h3>
              <span>This worker inspects another worker's tip and reports to the lead; it holds no plan step and no merge action.</span>
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
              <button class="button button-quiet" type="button" aria-label="Revert merged branch" onclick={() => actions.revertMerge(worker.id)}>Revert</button>
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
            <button class="button button-primary" type="button" aria-label="Merge reviewed branch" disabled={!mergeEligibility.ok || worker.status === "merging"} onclick={() => actions.mergeWorker(worker.id)}><GitMerge size={12} /> Merge reviewed branch</button>
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
  .worker-detail { height: 100%; overflow-y: auto; padding: 0 18px 18px; scrollbar-gutter: stable; }
  .worker-hero { position: sticky; top: 0; z-index: 10; padding-top: 10px; border-bottom: 1px solid var(--line); background: var(--canvas); }
  .worker-primary { display: flex; align-items: flex-start; justify-content: space-between; gap: 10px; }
  .worker-identity { display: flex; flex: 1; align-items: flex-start; gap: 8px; min-width: 0; }
  .avatar { display: grid; flex: 0 0 auto; width: 30px; height: 30px; place-items: center; border-radius: 8px; background: hsl(var(--hue) 32% var(--avatar-bg-lightness)); color: hsl(var(--hue) 30% var(--avatar-ink-lightness)); font-size: var(--text-meta); font-weight: 750; }
  .worker-heading { min-width: 0; }
  .worker-title { display: flex; min-height: 20px; align-items: center; flex-wrap: wrap; gap: 4px; }
  .worker-title h2 { margin: 0 2px 0 0; font-family: var(--font-ui); font-size: var(--text-title); font-weight: 650; letter-spacing: -0.02em; line-height: 20px; }
  .status-pill, .kind-badge, .effort-pill, .model-control { display: inline-flex; height: 19px; align-items: center; gap: 3px; padding: 0 5px; border: 1px solid var(--line); border-radius: 5px; background: var(--surface); color: var(--ink-soft); font-family: var(--font-ui); font-size: var(--text-meta); font-weight: 600; line-height: 1; white-space: nowrap; }
  .status-pill[data-status="in_review"], .status-pill[data-status="conflict"] { border-color: transparent; background: var(--warning-soft); color: var(--warning); }
  .status-pill[data-status="failed"] { border-color: transparent; background: var(--risk-bg); color: var(--danger); }
  .status-pill[data-status="merged"] { border-color: transparent; background: var(--accent-soft); color: var(--accent-strong); }
  .kind-badge { letter-spacing: 0.04em; text-transform: uppercase; }
  .kind-badge[data-kind="audit"] { border-color: transparent; background: var(--accent-soft); color: var(--accent-strong); }
  .kind-badge[data-risk="high"] { border-color: transparent; background: var(--risk-bg); color: var(--danger); }
  .model-control { padding: 0 4px; font-weight: 400; }
  .model-control :global(.model-effort) { height: 17px; gap: 3px; }
  .model-control :global(select) { height: 17px; padding-top: 0; padding-bottom: 0; font-size: var(--text-meta); }
  .model-locked { display: inline-flex; align-items: center; }
  .effort-pill { color: var(--muted); font-weight: 500; }
  .worker-plan { margin: 2px 0 0; overflow: hidden; color: var(--accent-strong); font-size: var(--text-meta); line-height: 14px; text-overflow: ellipsis; white-space: nowrap; }
  .worker-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 3px; max-width: 355px; }
  .toolbar-button { display: inline-flex; min-height: 23px; align-items: center; justify-content: center; gap: 3px; padding: 2px 6px; border: 1px solid var(--line); border-radius: 5px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: var(--text-body); font-weight: 520; line-height: 1; }
  .toolbar-button:hover:not(:disabled) { border-color: var(--line-strong); background: var(--surface-soft); color: var(--ink); }
  .toolbar-button:disabled { opacity: 0.4; cursor: not-allowed; }
  .toolbar-button.danger { color: var(--danger); }
  .worker-brief { min-width: 0; margin-top: 6px; }
  .brief-controls { display: flex; min-height: 12px; justify-content: flex-end; margin-bottom: 2px; }
  .brief-content { position: relative; overflow: hidden; }
  .brief-content.clamped::after { position: absolute; right: 0; bottom: 0; left: 0; height: 18px; background: linear-gradient(transparent, var(--canvas)); content: ""; pointer-events: none; }
  .worker-brief :global(.markdown) { font-size: var(--text-body); line-height: 1.4; }
  .worker-brief .brief-content :global(.markdown) :global(p) { margin-bottom: 5px; }
  .worker-brief .brief-content :global(.markdown) :global(h1),
  .worker-brief .brief-content :global(.markdown) :global(h2),
  .worker-brief .brief-content :global(.markdown) :global(h3) { margin: 0 0 5px; font-size: var(--text-body); line-height: 1.4; }
  .worker-brief .brief-content :global(.markdown) :global(ul),
  .worker-brief .brief-content :global(.markdown) :global(ol) { margin-bottom: 5px; }
  .brief-toggle { padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: var(--text-meta); font-weight: 650; line-height: 14px; }
  .brief-toggle:hover { text-decoration: underline; text-underline-offset: 2px; }
  .worker-meta { display: flex; min-height: 20px; align-items: center; justify-content: space-between; gap: 10px; margin-top: 4px; }
  .audit-scope { margin: 0; overflow: hidden; color: var(--muted); font-size: var(--text-meta); line-height: 1.4; text-overflow: ellipsis; white-space: nowrap; }
  .audit-scope code { font-family: var(--font-mono); font-size: var(--text-meta); }
  .file-areas { display: flex; flex-shrink: 0; align-items: center; gap: 4px; margin-left: auto; color: var(--muted); font-size: var(--text-meta); }
  .file-areas code { overflow: hidden; max-width: 180px; font-family: var(--font-mono); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .detail-tabs { display: flex; gap: 2px; margin-top: 4px; padding-top: 4px; border-top: 1px solid var(--line); }
  .detail-tabs button { display: inline-flex; height: 25px; align-items: center; gap: 4px; padding: 0 9px; border: 0; border-radius: 5px; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-body); font-weight: 570; }
  .detail-tabs button:hover { background: var(--surface-soft); color: var(--ink-soft); }
  .detail-tabs button.active { background: var(--surface-soft); color: var(--ink); }
  .detail-tabs button span { display: inline-grid; min-width: 13px; height: 13px; place-items: center; border-radius: 4px; background: var(--surface); color: var(--muted); font-size: var(--text-meta); }
  .detail-body { padding-top: 12px; }
  .worker-blocker { margin: 8px 0 0; padding: 7px 9px; border: 1px solid var(--warning-blocker-line); border-radius: 6px; background: var(--warning-soft); color: var(--ink-soft); font-size: var(--text-meta); line-height: 1.5; }
  .checks-panel, .merge-panel { padding: 10px 12px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); }
  .checks-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 7px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.08em; }
  .checks-heading code { overflow: hidden; font-family: var(--font-mono); font-size: var(--text-meta); letter-spacing: 0; text-overflow: ellipsis; white-space: nowrap; }
  .usage-line { margin: -3px 0 8px; color: var(--muted); font-size: var(--text-meta); }
  .check-row { display: flex; align-items: center; gap: 8px; padding: 5px 0; border-top: 1px solid var(--line); }
  .check-row > :global(svg) { color: var(--accent); }
  .check-row div { flex: 1; }
  .check-row strong, .check-row small { display: block; }
  .check-row strong { font-size: var(--text-meta); font-weight: 600; }
  .check-row small { margin-top: 2px; color: var(--muted); font-size: var(--text-meta); }
  .check-row > span { color: var(--muted); font-size: var(--text-meta); text-transform: uppercase; }
  .merge-panel { margin-top: 10px; }
  .merge-decision { display: flex; align-items: center; justify-content: space-between; gap: 14px; }
  .merge-decision p { margin: 0 0 4px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.09em; }
  .merge-decision h3 { margin: 0 0 4px; font-size: var(--text-body); font-weight: 650; }
  .merge-decision h3 code { font-family: var(--font-mono); }
  .merge-decision div > span { display: block; max-width: 420px; color: var(--muted); font-size: var(--text-meta); line-height: 1.45; }
  .merge-decision button { flex: 0 0 auto; min-height: 28px; padding: 5px 9px; font-size: var(--text-body); }
  .merge-reverted { color: var(--muted); font-size: var(--text-meta); }
  .merge-reverted code { font-family: var(--font-mono); }
  .timeline-step { display: grid; grid-template-columns: 20px 1fr; gap: 8px; min-height: 70px; }
  .timeline-marker { display: flex; justify-content: center; margin-left: 6px; border-left: 1px solid var(--line); }
  .timeline-marker > span { width: 8px; height: 8px; margin: 5px 0 0 -4.5px; border-radius: 50%; background: var(--line-strong); }
  .timeline-step[data-status="done"] .timeline-marker > span, .timeline-step[data-status="active"] .timeline-marker > span { background: var(--accent); }
  .timeline-step[data-status="active"] .timeline-marker > span { box-shadow: 0 0 0 4px var(--accent-soft); }
  .timeline-step strong { font-size: var(--text-body); }
  .timeline-step time { margin-left: 8px; color: var(--muted); font-size: var(--text-meta); }
  .timeline-step p { margin: 4px 0; color: var(--muted); font-size: var(--text-meta); }
  .timeline-step div > span { color: var(--muted); font-size: var(--text-meta); text-transform: uppercase; }
  @media (max-width: 900px) {
    .worker-primary { flex-direction: column; }
    .worker-actions { max-width: none; justify-content: flex-start; }
    .worker-meta { align-items: flex-start; flex-direction: column; gap: 4px; }
    .file-areas { margin-left: 0; }
  }
  @media (max-width: 620px) {
    .worker-detail { padding-right: 12px; padding-left: 12px; }
    .worker-title h2 { flex-basis: 100%; }
    .file-areas code { max-width: 220px; }
    .merge-decision { align-items: flex-start; flex-direction: column; }
  }
</style>
