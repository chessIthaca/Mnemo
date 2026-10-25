<script lang="ts">
  import {
    CheckCircle2,
    GitBranch,
    GitMerge,
    LoaderCircle,
    Pause,
    Play,
    Send,
    Square,
    XCircle,
  } from "@lucide/svelte";
  import { actions } from "../lib/store";
  import { workerStatusLabel, type Worker } from "../lib/types";
  import { deriveTaskWorkflow } from "../lib/workflow";
  import FindingsPanel from "./FindingsPanel.svelte";
  import ModelBadge from "./ModelBadge.svelte";
  import TaskWorkflow from "./TaskWorkflow.svelte";

  let { worker }: { worker: Worker } = $props();
  let tab = $state<"review" | "thread" | "timeline">("thread");
  let reply = $state("");
  let lastWorkerId = "";

  const openFindings = $derived(worker.findings.filter((finding) => finding.status === "open").length);
  const workflow = $derived(deriveTaskWorkflow(worker));

  $effect(() => {
    const id = worker.id;
    if (id === lastWorkerId) return;
    lastWorkerId = id;
    tab = "thread";
    reply = "";
  });

  function sendReply() {
    if (!reply.trim()) return;
    actions.replyWorker(worker.id, reply);
    reply = "";
  }

  function replyKeydown(event: KeyboardEvent) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      sendReply();
    }
  }
</script>

<article class="worker-detail">
  <header class="worker-hero">
    <div class="worker-identity">
      <span class="avatar" style="--hue: {worker.hue}">{worker.initials}</span>
      <div class="worker-heading">
        <div class="worker-title">
          <h2>{worker.name}<span data-status={worker.status}>{workerStatusLabel[worker.status]}</span></h2>
          <ModelBadge model={worker.model} />
        </div>
        <h3>{worker.task}</h3>
        <TaskWorkflow {workflow} />
      </div>
    </div>
    <div class="worker-actions">
      <button class="button button-quiet" onclick={() => actions.steerWorker(worker.id)}>Steer</button>
      {#if worker.status === "working" || worker.status === "dispatching"}
        <button class="button button-quiet" onclick={() => actions.pauseWorker(worker.id)}><Pause size={11} /> Pause</button>
      {:else if worker.status === "paused" || worker.status === "stopped" || worker.status === "interrupted"}
        <button class="button button-quiet" onclick={() => actions.resumeWorker(worker.id)}><Play size={11} /> Resume</button>
      {/if}
      <button class="button button-quiet" disabled={worker.status === "stopped"} onclick={() => actions.stopWorker(worker.id)}><Square size={11} /> Stop</button>
    </div>
  </header>

  <div class="file-areas"><GitBranch size={12} /><code>{worker.branch}</code><span>{worker.files.length} files</span></div>

  <nav class="detail-tabs" aria-label="Worker detail sections">
    <button class:active={tab === "thread"} aria-pressed={tab === "thread"} onclick={() => { tab = "thread"; }}>Thread <span>{worker.messages.length}</span></button>
    <button class:active={tab === "review"} aria-pressed={tab === "review"} onclick={() => { tab = "review"; }}>Review {#if openFindings > 0}<span>{openFindings}</span>{/if}</button>
    <button class:active={tab === "timeline"} aria-pressed={tab === "timeline"} onclick={() => { tab = "timeline"; }}>Timeline</button>
  </nav>

  <div class="detail-body">
    {#if tab === "review"}
      <section class="checks-panel">
        <div class="checks-heading"><span>BRANCH-TIP CHECKS</span><code>{worker.worktree}</code></div>
        {#each worker.checks as check (check.name)}
          <div class="check-row">
            {#if check.status === "passed"}<CheckCircle2 size={15} />{:else if check.status === "running"}<LoaderCircle size={15} />{:else}<XCircle size={15} />{/if}
            <div><strong>{check.name}</strong><small>{check.detail}</small></div>
            <span data-status={check.status}>{check.status}</span>
          </div>
        {/each}
      </section>
      <FindingsPanel {worker} />

      <section class="merge-panel">
        <div class="merge-decision">
          <div>
            <p>TEAMMANAGER · SERIALIZED MERGE</p>
            <h3>Merge into the lead branch</h3>
            <span>Phase 3 adds the real review gate, branch-tip checks, and the serialized merge. Nothing merges until then.</span>
          </div>
          <button class="button button-primary" aria-label="Merge reviewed branch" disabled={true} onclick={() => actions.mergeWorker(worker.id)}><GitMerge size={14} /> Merge reviewed branch</button>
        </div>
      </section>
    {:else if tab === "thread"}
      <section class="thread-panel" aria-label={`${worker.name} worker conversation`}>
        <div class="thread-messages">
          {#each worker.messages as message (message.id)}
            <div class="thread-message" class:from-lead={message.from === "lead"}>
              <span>{message.from === "lead" ? "LEAD" : worker.name.toUpperCase()} · {message.time}</span>
              <p>{message.body}</p>
            </div>
          {/each}
        </div>
        <div class="thread-composer">
          <textarea bind:value={reply} onkeydown={replyKeydown} rows="2" aria-label={`Reply to ${worker.name}`} placeholder={`Message ${worker.name}`}></textarea>
          <button class="icon-button" aria-label={`Send reply to ${worker.name}`} disabled={!reply.trim()} onclick={sendReply}><Send size={14} /></button>
        </div>
      </section>
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
  .worker-title { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .worker-identity h2 { display: flex; align-items: center; gap: 8px; margin: 0; font-family: var(--font-prose); font-size: 26px; font-weight: 550; }
  .worker-identity h2 span { padding: 4px 7px; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-ui); font-size: 7px; font-weight: 650; }
  .worker-identity h2 span[data-status="in_review"], .worker-identity h2 span[data-status="conflict"] { background: var(--warning-soft); color: var(--warning); }
  .worker-identity h3 { margin: 4px 0 0; color: var(--ink-soft); font-size: 10px; font-weight: 500; }
  .worker-title :global(.model-badge) { max-width: 180px; overflow: hidden; text-overflow: ellipsis; }
  .worker-heading :global(.task-workflow) { margin-top: 7px; max-width: 470px; }
  .worker-actions { display: flex; gap: 7px; }
  .file-areas code { margin-right: 4px; color: var(--ink-soft); font-family: var(--font-mono); font-size: 8px; }
  .file-areas span { padding: 4px 6px; border-radius: 5px; background: var(--surface-soft); font-family: var(--font-mono); font-size: 7px; }
  .detail-tabs { display: flex; gap: 18px; border-bottom: 1px solid var(--line); }
  .detail-tabs button { display: inline-flex; align-items: center; gap: 5px; padding: 0 0 10px; border: 0; border-bottom: 2px solid transparent; background: transparent; color: var(--muted); cursor: pointer; font-size: 10px; font-weight: 600; }
  .detail-tabs button.active { border-color: var(--accent); color: var(--ink); }
  .detail-tabs button span { display: grid; min-width: 15px; height: 15px; place-items: center; border-radius: 8px; background: var(--surface-soft); font-size: 7px; }
  .detail-body { display: grid; gap: 12px; padding-top: 16px; }
  .checks-panel, .merge-panel { padding: 14px 16px; border: 1px solid var(--line); border-radius: 13px; background: var(--surface); }
  .checks-heading { display: flex; justify-content: space-between; gap: 10px; margin-bottom: 10px; color: var(--muted); font-size: 7px; letter-spacing: 0.08em; }
  .checks-heading code { overflow: hidden; font-family: var(--font-mono); font-size: 7px; letter-spacing: 0; text-overflow: ellipsis; white-space: nowrap; }
  .check-row { display: flex; align-items: center; gap: 9px; }
  .check-row > :global(svg) { color: var(--accent); }
  .check-row div { flex: 1; }
  .check-row strong, .check-row small { display: block; }
  .check-row strong { font-size: 10px; font-weight: 600; }
  .thread-message > span { color: var(--muted); font-size: 7px; letter-spacing: 0.07em; }

  .merge-decision { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
  .merge-decision p { margin: 0 0 4px; color: var(--muted); font-size: 7px; letter-spacing: 0.09em; }
  .merge-decision h3 { margin: 0 0 4px; font-size: 12px; font-weight: 650; }
  .merge-decision div > span { display: block; max-width: 360px; color: var(--muted); font-size: 8px; line-height: 1.45; }
  .merge-decision button { flex: 0 0 auto; }
  .thread-message p { margin: 5px 0 0; padding: 10px 12px; border-radius: 4px 12px 12px 12px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-prose); font-size: 13px; line-height: 1.5; }
  .thread-message.from-lead { margin-left: auto; }
  .thread-message.from-lead > span { display: block; text-align: right; }
  .thread-message.from-lead p { border-radius: 12px 12px 4px 12px; background: var(--accent-soft); }
  .thread-composer { display: flex; align-items: flex-end; gap: 8px; margin-top: 16px; padding: 9px; border: 1px solid var(--line-strong); border-radius: 12px; background: var(--surface); }
  .thread-composer textarea { flex: 1; resize: none; border: 0; outline: 0; color: var(--ink); font-size: 10px; }
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
