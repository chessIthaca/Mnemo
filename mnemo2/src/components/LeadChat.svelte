<script lang="ts">
  import { onMount } from "svelte";
  import { ArrowUpRight, ClipboardCheck, MessageSquareText } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import { workerStatusText } from "../lib/types";
  import { selectNeedsYouWorkers, selectRunningWorkers } from "../lib/workerSelection";
  import Composer from "./Composer.svelte";
  import ExternalLeadPanel from "./ExternalLeadPanel.svelte";
  import Markdown from "./Markdown.svelte";
  import EventRow from "./EventRow.svelte";
  import MergeCard from "./MergeCard.svelte";
  import PendingPrompt from "./PendingPrompt.svelte";
  import ToolActivity from "./ToolActivity.svelte";
  import ActivityStatus from "./ActivityStatus.svelte";

  const STICK_TO_BOTTOM_PX = 48;
  const runningTool = $derived.by(() => {
    const turnStartedAtMs = $app.leadRuntime.turnStartedAtMs;
    return $app.leadEntries.some((entry) =>
      entry.kind === "tool" && entry.result === null &&
      (turnStartedAtMs === null || entry.startedAtMs === undefined || entry.startedAtMs >= turnStartedAtMs)
    );
  });
  const showThinking = $derived(
    $app.leadRuntime.busy &&
      !$app.leadRuntime.pending &&
      !$app.leadRuntime.error &&
      !runningTool,
  );

  // A lead message with no text (the model often emits one right before a
  // tool call) would only add a gap.
  const entries = $derived(
    $app.leadEntries.filter((entry) => !(entry.kind === "message" && entry.author === "lead" && !entry.body.trim())),
  );

  // The lead ends its turn while workers run; TeamManager wakes it when one
  // finishes. Say so, so an idle lead does not read as a stalled one.
  const runningWorkers = $derived(selectRunningWorkers($app.workers));
  // Same predicate as needsYouCount: any worker waiting on a human, busy lead or not.
  const needsYouWorkers = $derived(selectNeedsYouWorkers($app.workers));
  const showWaiting = $derived(
    !$app.leadRuntime.busy && !$app.leadRuntime.pending && !$app.leadRuntime.error && runningWorkers.length > 0,
  );

  // While Claude Code leads externally, a pointer panel replaces the idle
  // built-in chat; "Open the built-in lead anyway" reveals it behind a slim
  // bar. Any attach or detach returns the view to that default.
  let externalRevealed = $state(false);
  const showExternalPanel = $derived($app.externalLead.attached && !externalRevealed);
  const showExternalBar = $derived($app.externalLead.attached && externalRevealed);

  $effect(() => {
    void $app.externalLead.attached;
    externalRevealed = false;
  });

  let scroller = $state<HTMLDivElement>();
  let content = $state<HTMLDivElement>();
  let nearBottom = true;
  let scrollFrame: number | null = null;
  let resizeObserver: ResizeObserver | null = null;

  onMount(() => {
    if (typeof ResizeObserver !== "undefined") {
      resizeObserver = new ResizeObserver(() => scheduleScroll());
      if (content) resizeObserver.observe(content);
    }
    scheduleScroll();
    return () => {
      resizeObserver?.disconnect();
      resizeObserver = null;
      if (scrollFrame !== null) {
        cancelAnimationFrame(scrollFrame);
        scrollFrame = null;
      }
    };
  });

  $effect(() => {
    $app.leadEntries.length;
    $app.leadRuntime.pending;
    scheduleScroll();
  });

  // The external-lead panel unmounts the transcript, so re-observe the
  // live content and settle at the bottom whenever the chat is shown.
  $effect(() => {
    if (showExternalPanel) return;
    const stream = content;
    if (!stream) return;
    resizeObserver?.observe(stream);
    scheduleScroll();
    return () => {
      resizeObserver?.unobserve(stream);
    };
  });

  function handleScroll() {
    if (!scroller) return;
    const distance = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    nearBottom = distance <= STICK_TO_BOTTOM_PX;
  }

  function scheduleScroll() {
    if (!scroller || scrollFrame !== null) return;
    scrollFrame = requestAnimationFrame(() => {
      scrollFrame = null;
      if (!scroller || !nearBottom) return;
      scroller.scrollTop = scroller.scrollHeight;
    });
  }
</script>

<section class="lead-workspace" aria-label="Lead conversation">
  {#if showExternalPanel}
    <ExternalLeadPanel onReveal={() => (externalRevealed = true)} />
  {:else}
    {#if showExternalBar}
      <div class="external-lead-bar">
        <span>Claude Code is leading this project.</span>
        <button type="button" onclick={() => (externalRevealed = false)}>Back</button>
      </div>
    {/if}
  <div class="lead-scroll" bind:this={scroller} onscroll={handleScroll}>
    <div class="lead-content">
      <div class="lead-stream" bind:this={content}>
      <div class="lead-entries">
        {#each entries as entry, index (entry.id)}
          {#if entry.kind === "message"}
            <article
              class:user-entry={entry.author === "user"}
              class:before-tool={entries[index + 1]?.kind === "tool"}
              class="chat-entry"
            >
              {#if entry.author === "lead"}
                <Markdown source={entry.body} onOpenLink={actions.openBrowser} onOpenPath={actions.openRepoPath} />
              {:else}
                {#if entry.body}<p>{entry.body}</p>{/if}
                {#if entry.images && entry.images.length > 0}
                  <div class="user-images" aria-label="Attached images">
                    {#each entry.images as image, imageIndex}
                      <img src={image} alt={`Attachment ${imageIndex + 1}`} />
                    {/each}
                  </div>
                {/if}
                <div class="user-meta">YOU</div>
              {/if}
            </article>
          {:else if entry.kind === "tool"}
            <div
              class="tool-activity-row"
              class:tool-connected={entries[index + 1]?.kind === "tool"}
              class:before-message={entries[index + 1]?.kind === "message"}
            >
              <ToolActivity {entry} />
            </div>
          {:else if entry.kind === "team-message"}
            {@const name = $app.workers.find((item) => item.id === entry.workerId)?.name ?? entry.workerName ?? entry.workerId}
            <EventRow label={`${name} sent an update`}>
              {#snippet icon()}<MessageSquareText size={13} />{/snippet}
              {#snippet heading()}{name} sent an update{/snippet}
              <div class="event-detail">
                <span class="event-time">{entry.time}</span>
                <Markdown source={entry.body} onOpenLink={actions.openBrowser} onOpenPath={actions.openRepoPath} />
                <button onclick={() => actions.selectWorker(entry.workerId)}>Open {name}’s thread <ArrowUpRight size={13} /></button>
              </div>
            </EventRow>
          {:else if entry.kind === "review"}
            {@const worker = $app.workers.find((item) => item.id === entry.workerId)}
            {@const evidence = worker?.review?.evidence}
            {@const name = worker?.name ?? entry.workerName ?? entry.workerId}
            <EventRow
              tone="review"
              label={entry.title.includes(name) ? entry.title : `${entry.title} · ${name}`}
            >
              {#snippet icon()}<ClipboardCheck size={13} />{/snippet}
              {#snippet heading()}{entry.title}{#if !entry.title.includes(name)}<span class="event-subject">{` · ${name}`}</span>{/if}{/snippet}
              <div class="event-detail">
                <span class="event-time">{entry.time}</span>
                {#if worker?.reviewRisk === "high"}
                  <span class="review-risk" data-risk="high">High risk</span>
                  <p class="review-audit" data-audited={Boolean(evidence?.independentReviewerId)}>
                    {#if evidence?.independentReviewerId}
                      Independent audit linked at the reviewed tip.
                    {:else}
                      No independent audit linked yet — this merge is blocked until the lead adjudicates one.
                    {/if}
                  </p>
                {:else if worker?.review && !evidence}
                  <p class="review-audit" data-audited="legacy">
                    Legacy review · structured evidence is missing, so a fresh review is required before merge.
                  </p>
                {/if}
                <Markdown source={entry.body} onOpenLink={actions.openBrowser} onOpenPath={actions.openRepoPath} />
                <button onclick={() => actions.selectWorker(entry.workerId)}>Review {name}’s delta <ArrowUpRight size={13} /></button>
              </div>
            </EventRow>
          {:else if entry.kind === "merge"}
            <MergeCard {entry} />
          {/if}
        {/each}
        {#if showThinking}
          <!-- The wrapper carries the chat's indent (`.thinking-status` margin); the shared
               component inside renders the same dot, label, and timer the waiting rows use. -->
          <div class="thinking-status">
            <ActivityStatus label="Thinking…" startedAtMs={$app.leadRuntime.turnStartedAtMs} />
          </div>
        {/if}
        {#if showWaiting}
          <div class="thinking-status waiting-status" role="status" aria-live="polite">
            <span class="thinking-dot" aria-hidden="true"></span>
            <span>{`Waiting on `}{#each runningWorkers as worker, index (worker.id)}{#if index > 0}{`, `}{/if}<button class="waiting-worker" type="button" aria-label={`Show ${worker.name}'s output`} onclick={() => actions.openWorkerPanel(worker.id)}>{worker.name}</button>{/each}{`. The lead picks up when ${runningWorkers.length === 1 ? "it finishes" : "one finishes"}.`}</span>
          </div>
        {/if}
        {#if needsYouWorkers.length > 0}
          <div class="thinking-status needs-you-status" role="status" aria-live="polite">
            <span class="thinking-dot" aria-hidden="true"></span>
            <span>{`Needs you: `}{#each needsYouWorkers as worker, index (worker.id)}{#if index > 0}{`, `}{/if}<button class="waiting-worker" type="button" aria-label={`Open ${worker.name}`} onclick={() => actions.openWorkerPanel(worker.id)}>{worker.name}</button>{` (${workerStatusText(worker)})`}{/each}{`.`}</span>
          </div>
        {/if}
      </div>

      {#if $app.leadRuntime.error}
        <article class="lead-notice lead-error" role="alert">
          <div class="notice-label">{$app.leadRuntime.errorRetrying ? "RETRYING" : "LEAD ERROR"}</div>
          <p>{$app.leadRuntime.error}</p>
          {#if $app.leadRuntime.errorRetrying}
            <p class="notice-detail">The lead is retrying automatically.</p>
          {:else}
            <div class="notice-actions">
              <button class="button button-quiet" onclick={() => actions.retryLead()}>Retry</button>
            </div>
          {/if}
        </article>
      {/if}

      {#if $app.leadRuntime.pending && !["question", "approval"].includes($app.leadRuntime.pending.kind)}
        {@const pending = $app.leadRuntime.pending}
        <article class="lead-notice lead-pending" role="status">
          {#if pending.kind === "host_tool_call"}
            <div class="notice-label">TEAM WORK IN PROGRESS</div>
            <p>The team runtime is executing {pending.tool_name ?? "a host tool"}. No approval response is required.</p>
          {:else}
            <div class="notice-label">PAUSED</div>
            <p>The lead is paused on a {String(pending.kind).replaceAll("_", " ")} that has no answer control here. Stop the turn to continue.</p>
          {/if}
        </article>
      {/if}
      </div>
    </div>
  </div>
  <footer class="composer-dock"><PendingPrompt /><Composer /></footer>
  {/if}
</section>

<style>
  .lead-workspace { display: flex; min-width: 0; min-height: 0; flex: 1; flex-direction: column; container-type: inline-size; background: var(--canvas); }
  .lead-scroll { flex: 1; min-width: 0; min-height: 0; overflow-y: auto; scrollbar-gutter: stable; }
  .lead-content { width: 100%; min-width: 0; margin: 0; padding: 22px 32px 18px; }
  .lead-stream { display: flow-root; min-width: 0; }
  .chat-entry { margin: 0 0 16px; }
  /* A tool run reads as part of the message that started it: keep it close,
     then give the next message room to start fresh. */
  .chat-entry.before-tool:not(.user-entry) { margin-bottom: 6px; }
  .chat-entry :global(.markdown > :last-child) { margin-bottom: 0; }
  .chat-entry:not(.user-entry) { margin-left: 14px; }
  .chat-entry :global(.markdown p), .chat-entry :global(.markdown li), .chat-entry :global(.markdown blockquote) { max-width: 100ch; }
  .thinking-status { display: flex; align-items: center; gap: 6px; margin: 6px 0 12px 30px; color: var(--muted); font-size: var(--text-body); }
  .thinking-dot { width: 5px; height: 5px; border-radius: 50%; background: var(--accent); animation: thinking-pulse 1.2s ease-in-out infinite; }
  .waiting-status .thinking-dot { animation-duration: 2.4s; }
  .waiting-worker { padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font: inherit; font-weight: 650; }
  .waiting-worker:hover { text-decoration: underline; }
  .needs-you-status .thinking-dot { animation-duration: 0.9s; background: var(--danger); }
  @keyframes thinking-pulse { 0%, 100% { opacity: 0.35; } 50% { opacity: 1; } }
  @media (prefers-reduced-motion: reduce) {
    .thinking-dot { animation: none; opacity: 1; }
  }
  .user-entry { max-width: 100ch; margin: 20px 0; padding: 2px 0 2px 12px; border-left: 2px solid var(--line-strong); }
  .user-entry p { margin: 0; color: var(--ink-soft); font-size: var(--text-body); line-height: 1.6; white-space: pre-wrap; }
  .user-images { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 8px; }
  .user-images img { width: 72px; height: 72px; border: 1px solid var(--line); border-radius: 10px; object-fit: cover; }
  .user-meta { margin-top: 4px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.08em; text-align: left; }
  .tool-activity-row { position: relative; min-width: 0; margin-left: 30px; }
  .tool-activity-row.before-message { margin-bottom: 18px; }
  .tool-activity-row.tool-connected::after { position: absolute; top: 24px; bottom: 0; left: 9px; border-left: 1px dotted var(--line-strong); content: ""; pointer-events: none; }
  .event-detail :global(.markdown) { margin: 4px 0 8px; color: var(--ink-soft); font-size: var(--text-body); line-height: 1.55; }
  .event-detail button { display: inline-flex; align-items: center; gap: 5px; padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: var(--text-body); font-weight: 650; }
  .event-time { color: var(--muted); font-size: var(--text-meta); }
  .event-subject { color: var(--muted); font-weight: 500; }
  .review-risk { display: inline-block; margin: 0 0 6px; padding: 2px 6px; border-radius: 5px; background: var(--risk-bg); color: var(--danger); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.04em; }
  .review-audit { margin: 0 0 9px; padding: 6px 9px; border-radius: 8px; background: var(--surface); color: var(--muted); font-size: var(--text-meta); line-height: 1.45; }
  .review-audit[data-audited="true"] { color: var(--accent-strong); }
  .review-audit[data-audited="false"], .review-audit[data-audited="legacy"] { color: var(--warning); }
  .lead-notice { margin: 0 0 12px; padding: 12px 14px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
  .lead-notice p { margin: 0; color: var(--ink-soft); font-size: var(--text-body); line-height: 1.5; }
  .lead-notice .notice-detail { margin-top: 5px; color: var(--muted); font-size: var(--text-body); }
  .lead-error { border-color: var(--danger-tint-line); background: var(--danger-tint); }
  .lead-error p { color: var(--danger); }
  .lead-pending { border-color: var(--warning-line); background: var(--warning-pending); }
  .notice-label { margin-bottom: 5px; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.08em; }
  .notice-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 10px; }
  .external-lead-bar { display: flex; flex: 0 0 auto; align-items: center; justify-content: space-between; gap: 12px; padding: 7px 32px; border-bottom: 1px solid var(--line); background: var(--surface); color: var(--ink-soft); font-size: var(--text-body); }
  .external-lead-bar button { padding: 3px 10px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font: inherit; font-size: var(--text-meta); }
  .external-lead-bar button:hover { border-color: var(--line-strong); color: var(--ink); }
  .composer-dock { flex: 0 0 auto; padding: 10px 32px 12px; background: linear-gradient(transparent, var(--canvas) 18%); }
  @media (max-width: 760px) {
    .lead-content { padding: 16px; }
    .composer-dock { padding: 10px 16px 72px; }
    .external-lead-bar { padding: 7px 16px; }
  }
  @media (max-width: 560px) {
    .chat-entry:not(.user-entry), .tool-activity-row { margin-left: 0; }
  }
  /* The side panel, not the window, narrows the lead — so the gutters follow the
     lead column's own width (container query, not a viewport media query). */
  @container (max-width: 600px) {
    .lead-content, .composer-dock, .external-lead-bar { padding-left: 16px; padding-right: 16px; }
  }
</style>
