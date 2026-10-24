<script lang="ts">
  import {
    CheckCircle2,
    GitBranch,
    GitMerge,
    LoaderCircle,
    Send,
    Square,
    XCircle,
  } from "@lucide/svelte";
  import { canMerge } from "../lib/review";
  import { actions, currentProject } from "../lib/store";
  import { workerStatusLabel, type Worker } from "../lib/types";
  import FindingsPanel from "./FindingsPanel.svelte";

  let { worker }: { worker: Worker } = $props();
  let tab = $state<"review" | "thread" | "timeline">("review");
  let reply = $state("");

  const eligibility = $derived(canMerge(worker));
  const openFindings = $derived(worker.findings.filter((finding) => finding.status === "open").length);

  $effect(() => {
    worker.id;
    worker.findings.length;
    tab = worker.findings.length > 0 ? "review" : "thread";
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
      <div>
        <p>WORKER / {worker.branch}</p>
        <h2>{worker.name}<span data-status={worker.status}>{workerStatusLabel[worker.status]}</span></h2>
        <h3>{worker.task}</h3>
      </div>
    </div>
    <div class="worker-actions">
      <button class="button button-quiet" onclick={() => actions.steerWorker(worker.id)}>Steer</button>
      <button class="button button-quiet" disabled={worker.status === "merged" || worker.status === "cancelled"} onclick={() => actions.stopWorker(worker.id)}><Square size={11} /> Stop</button>
    </div>
  </header>

  <p class="worker-summary">{worker.summary}</p>
  <div class="worker-metrics">
    <span><small>MODEL</small><strong>{worker.model}</strong></span>
    <span><small>COST</small><strong>${worker.cost.toFixed(2)}</strong></span>
    <span><small>TOKENS</small><strong>{worker.tokens.toLocaleString()}</strong></span>
    <span><small>ELAPSED</small><strong>{worker.elapsed}</strong></span>
    <span><small>DIFF</small><strong><i>+{worker.diff.additions}</i> <b>−{worker.diff.deletions}</b></strong></span>
  </div>
  <div class="worker-progress"><span style={`width: ${worker.progress}%`}></span></div>
  <div class="file-areas"><GitBranch size={12} /><code>{worker.branch}</code>{#each worker.files as file}<span>{file}</span>{/each}</div>

  <nav class="detail-tabs" aria-label="Worker detail sections">
    <button class:active={tab === "review"} aria-pressed={tab === "review"} onclick={() => { tab = "review"; }}>Review {#if openFindings > 0}<span>{openFindings}</span>{/if}</button>
    <button class:active={tab === "thread"} aria-pressed={tab === "thread"} onclick={() => { tab = "thread"; }}>Thread <span>{worker.messages.length}</span></button>
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
        {#if worker.merge}
          {@const record = worker.merge}
          <div class="merge-record">
            <span class="merge-record-icon"><GitMerge size={18} /></span>
            <div><p>MERGE RECORD</p><h3>Merged into {record.target}</h3><code>{record.commit} · {record.branch}</code><span>{record.reviewSummary} · {record.checks.length} check passed · ${record.cost.toFixed(2)}</span></div>
          </div>
        {:else}
          <div class="merge-decision">
            <div><p>TEAMMANAGER · SERIALIZED MERGE</p><h3>Merge into {$currentProject?.branch ?? "wt/mnemo"}</h3><span>{#if eligibility.ok}Review closed and checks passed on this tip. The lead is idle; the tree is clean.{:else}{eligibility.reason}{/if}</span></div>
            <button class="button button-primary" aria-label="Merge reviewed branch" disabled={!eligibility.ok} onclick={() => actions.mergeWorker(worker.id)}><GitMerge size={14} /> Merge reviewed branch</button>
          </div>
        {/if}
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
          <textarea bind:value={reply} onkeydown={replyKeydown} rows="2" aria-label={`Reply to ${worker.name}`} placeholder={`Guide ${worker.name} without opening the tool log…`}></textarea>
          <button class="icon-button" aria-label={`Send reply to ${worker.name}`} disabled={!reply.trim()} onclick={sendReply}><Send size={14} /></button>
        </div>
        <p class="thread-note">Questions route through the lead. Workers never interrupt you directly.</p>
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
  .worker-identity p { margin: 0 0 4px; overflow: hidden; color: var(--muted); font-family: var(--font-mono); font-size: 7px; letter-spacing: 0.06em; text-overflow: ellipsis; white-space: nowrap; }
  .worker-identity h2 { display: flex; align-items: center; gap: 8px; margin: 0; font-family: var(--font-prose); font-size: 26px; font-weight: 550; }
  .worker-identity h2 span { padding: 4px 7px; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-ui); font-size: 7px; font-weight: 650; }
  .worker-identity h2 span[data-status="in_review"], .worker-identity h2 span[data-status="conflict"] { background: var(--warning-soft); color: var(--warning); }
  .worker-identity h3 { margin: 4px 0 0; color: var(--ink-soft); font-size: 10px; font-weight: 500; }
  .worker-actions { display: flex; gap: 7px; }
  .worker-summary { margin: 18px 0; color: var(--ink-soft); font-family: var(--font-prose); font-size: 14px; line-height: 1.6; }
  .worker-metrics { display: grid; grid-template-columns: repeat(5, 1fr); gap: 10px; padding: 12px 0; border-top: 1px solid var(--line); border-bottom: 1px solid var(--line); }
  .worker-metrics small, .worker-metrics strong { display: block; }
  .worker-metrics small { margin-bottom: 4px; color: var(--muted); font-size: 7px; letter-spacing: 0.08em; }
  .worker-metrics strong { font-size: 10px; font-weight: 600; }
  .worker-metrics i, .worker-metrics b { font-style: normal; font-weight: 600; }
  .worker-metrics i { color: #4f7d48; } .worker-metrics b { color: #a15b53; }
  .worker-progress { height: 2px; margin-top: 10px; background: var(--line); }
  .worker-progress span { display: block; height: 100%; background: var(--accent); }
  .file-areas { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin: 12px 0 18px; color: var(--muted); }
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
  .check-row small { margin-top: 3px; color: var(--muted); font-size: 8px; }
  .check-row > span { color: var(--muted); font-size: 8px; }
  .merge-decision { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
  .merge-decision p, .merge-record p { margin: 0 0 4px; color: var(--muted); font-size: 7px; letter-spacing: 0.09em; }
  .merge-decision h3, .merge-record h3 { margin: 0 0 4px; font-size: 12px; font-weight: 650; }
  .merge-decision div > span { display: block; max-width: 360px; color: var(--muted); font-size: 8px; line-height: 1.45; }
  .merge-decision button { flex: 0 0 auto; }
  .merge-record { display: flex; align-items: center; gap: 12px; }
  .merge-record-icon { display: grid; width: 38px; height: 38px; place-items: center; border-radius: 11px; background: var(--accent-soft); color: var(--accent-strong); }
  .merge-record code, .merge-record div > span { display: block; margin-top: 3px; color: var(--muted); font-family: var(--font-mono); font-size: 8px; }
  .thread-messages { display: grid; gap: 12px; max-height: 330px; overflow-y: auto; padding: 4px; }
  .thread-message { max-width: 88%; }
  .thread-message > span { color: var(--muted); font-size: 7px; letter-spacing: 0.07em; }
  .thread-message p { margin: 5px 0 0; padding: 10px 12px; border-radius: 4px 12px 12px 12px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-prose); font-size: 13px; line-height: 1.5; }
  .thread-message.from-lead { margin-left: auto; }
  .thread-message.from-lead > span { display: block; text-align: right; }
  .thread-message.from-lead p { border-radius: 12px 12px 4px 12px; background: var(--accent-soft); }
  .thread-composer { display: flex; align-items: flex-end; gap: 8px; margin-top: 16px; padding: 9px; border: 1px solid var(--line-strong); border-radius: 12px; background: var(--surface); }
  .thread-composer textarea { flex: 1; resize: none; border: 0; outline: 0; color: var(--ink); font-size: 10px; }
  .thread-note { color: var(--muted); font-size: 8px; text-align: center; }
  .timeline-step { display: grid; grid-template-columns: 20px 1fr; gap: 8px; min-height: 76px; }
  .timeline-marker { display: flex; justify-content: center; border-left: 1px solid var(--line); margin-left: 6px; }
  .timeline-marker > span { width: 8px; height: 8px; margin: 5px 0 0 -4.5px; border-radius: 50%; background: var(--line-strong); }
  .timeline-step[data-status="done"] .timeline-marker > span { background: var(--accent); }
  .timeline-step[data-status="active"] .timeline-marker > span { background: var(--accent); box-shadow: 0 0 0 4px var(--accent-soft); }
  .timeline-step strong { font-size: 11px; }
  .timeline-step time { margin-left: 8px; color: var(--muted); font-size: 8px; }
  .timeline-step p { margin: 4px 0; color: var(--muted); font-size: 9px; }
  .timeline-step div > span { color: var(--muted); font-size: 7px; text-transform: uppercase; }
  @media (max-width: 1100px) { .worker-metrics { grid-template-columns: repeat(3, 1fr); } .merge-decision { align-items: flex-start; flex-direction: column; } }
  @media (max-width: 620px) { .worker-detail { padding: 16px; } .worker-hero { flex-direction: column; } .worker-metrics { grid-template-columns: repeat(2, 1fr); } }
</style>
