<script lang="ts">
  import { onMount } from "svelte";
  import { ArrowUpRight, ClipboardCheck, MessageSquareText } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import Composer from "./Composer.svelte";
  import Markdown from "./Markdown.svelte";
  import MergeCard from "./MergeCard.svelte";
  import ToolActivity from "./ToolActivity.svelte";
  import ElapsedTime from "./ElapsedTime.svelte";

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

  let scroller: HTMLDivElement;
  let content: HTMLDivElement;
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
  <div class="lead-scroll" bind:this={scroller} onscroll={handleScroll}>
    <div class="lead-content">
      <div class="lead-stream" bind:this={content}>
      <div class="lead-entries">
        {#each $app.leadEntries as entry, index (entry.id)}
          {#if entry.kind === "message"}
            <article class:user-entry={entry.author === "user"} class="chat-entry">
              {#if entry.author === "lead"}
                <div class="entry-label">LEAD</div>
                <Markdown source={entry.body} onOpenLink={actions.openBrowser} />
              {:else}
                <p>{entry.body}</p>
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
            <div class="tool-activity-row" class:tool-connected={$app.leadEntries[index + 1]?.kind === "tool"}>
              <ToolActivity {entry} />
            </div>
          {:else if entry.kind === "team-message"}
            {@const worker = $app.workers.find((item) => item.id === entry.workerId)}
            <article class="team-card">
              <div class="team-card-heading">
                <span class="team-card-icon"><MessageSquareText size={15} /></span>
                <div><strong>{worker?.name ?? "Worker"} sent an update</strong><span>TEAM MESSAGE · {entry.time}</span></div>
              </div>
              <Markdown source={entry.body} onOpenLink={actions.openBrowser} />
              <button onclick={() => actions.selectWorker(entry.workerId)}>Open worker thread <ArrowUpRight size={13} /></button>
            </article>
          {:else if entry.kind === "review"}
            {@const worker = $app.workers.find((item) => item.id === entry.workerId)}
            {@const evidence = worker?.review?.evidence}
            <article class="team-card review-card">
              <div class="team-card-heading">
                <span class="team-card-icon"><ClipboardCheck size={15} /></span>
                <div>
                  <strong>{entry.title}</strong>
                  <span>LEAD REVIEW · {entry.time}</span>
                  {#if worker?.reviewRisk === "high"}
                    <span class="review-risk" data-risk="high">High risk</span>
                  {/if}
                </div>
              </div>
              <Markdown source={entry.body} onOpenLink={actions.openBrowser} />
              {#if worker && worker.reviewRisk === "high"}
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
              <button onclick={() => actions.selectWorker(entry.workerId)}>Review {worker?.name ?? "worker"}’s delta <ArrowUpRight size={13} /></button>
            </article>
          {:else if entry.kind === "merge"}
            <MergeCard {entry} />
          {/if}
        {/each}
        {#if showThinking}
          <div class="thinking-status" role="status" aria-live="off">
            <span class="thinking-dot" aria-hidden="true"></span>
            <span>Thinking…</span>
            {#if $app.leadRuntime.turnStartedAtMs !== null}
              <ElapsedTime startedAtMs={$app.leadRuntime.turnStartedAtMs} />
            {/if}
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

      {#if $app.leadRuntime.pending}
        {@const pending = $app.leadRuntime.pending}
        <article class="lead-notice lead-pending" role="status">
          {#if pending.kind === "approval"}
            <div class="notice-label">APPROVAL REQUIRED</div>
            <p>{pending.tool_name ? `${pending.tool_name} is waiting for approval.` : "A gated action is waiting for approval."}</p>
            <div class="notice-actions">
              <button class="button button-primary" disabled={!pending.request_id} onclick={() => pending.request_id && actions.approveLead(pending.request_id, "approve")}>Approve</button>
              <button class="button button-quiet" disabled={!pending.request_id} onclick={() => pending.request_id && actions.approveLead(pending.request_id, "deny")}>Deny</button>
            </div>
          {:else if pending.kind === "question"}
            <div class="notice-label">QUESTION</div>
            <p>{pending.question ?? "The lead needs an answer."}</p>
            <div class="notice-actions">
              {#each pending.options ?? [] as option, index}
                <button class="button button-quiet" disabled={!pending.question_id} onclick={() => pending.question_id && actions.answerLead(pending.question_id, { kind: "choice", index })}>{option.label}</button>
              {/each}
            </div>
          {:else if pending.kind === "host_tool_call"}
            <div class="notice-label">TEAM WORK IN PROGRESS</div>
            <p>The team runtime is executing {pending.tool_name ?? "a host tool"}. No approval response is required.</p>
          {:else}
            <div class="notice-label">WAITING FOR YOU</div>
            <p>The lead needs input to continue.</p>
          {/if}
        </article>
      {/if}
      </div>
    </div>
  </div>
  <footer class="composer-dock"><Composer /></footer>
</section>

<style>
  .lead-workspace { display: flex; min-width: 0; min-height: 0; flex: 1; flex-direction: column; background: var(--canvas); }
  .lead-scroll { flex: 1; min-width: 0; min-height: 0; overflow-y: auto; scrollbar-gutter: stable; }
  .lead-content { width: 100%; min-width: 0; margin: 0; padding: 22px 32px 18px; }
  .lead-stream { display: flow-root; min-width: 0; }
  .chat-entry { margin: 0 0 16px; }
  .chat-entry:not(.user-entry) { margin-left: 14px; }
  .chat-entry :global(.markdown p), .chat-entry :global(.markdown li), .chat-entry :global(.markdown blockquote) { max-width: 100ch; }
  .entry-label { display: flex; align-items: center; gap: 6px; margin-bottom: 7px; color: var(--muted); font-size: 8px; font-weight: 650; letter-spacing: 0.08em; }
  .thinking-status { display: flex; align-items: center; gap: 6px; margin: 6px 0 12px 30px; color: var(--muted); font-size: 10px; }
  .thinking-dot { width: 5px; height: 5px; border-radius: 50%; background: var(--accent); animation: thinking-pulse 1.2s ease-in-out infinite; }
  @keyframes thinking-pulse { 0%, 100% { opacity: 0.35; } 50% { opacity: 1; } }
  .user-entry { max-width: 100ch; margin: 20px 0; padding: 2px 0 2px 12px; border-left: 2px solid var(--line-strong); }
  .user-entry p { margin: 0; color: var(--ink-soft); font-size: 13px; line-height: 1.6; white-space: pre-wrap; }
  .user-images { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 8px; }
  .user-images img { width: 72px; height: 72px; border: 1px solid var(--line); border-radius: 10px; object-fit: cover; }
  .user-meta { margin-top: 4px; color: var(--muted); font-size: 8px; letter-spacing: 0.08em; text-align: left; }
  .tool-activity-row { position: relative; min-width: 0; margin-left: 30px; }
  .tool-activity-row.tool-connected::after { position: absolute; top: 24px; bottom: 0; left: 9px; border-left: 1px dotted var(--line-strong); content: ""; pointer-events: none; }
  .team-card { margin: 10px 0 10px 30px; padding: 11px 14px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface-soft); }
  .team-card-heading { display: flex; align-items: center; gap: 10px; }
  .team-card-icon { display: grid; width: 29px; height: 29px; place-items: center; border-radius: 9px; background: var(--surface); color: var(--accent-strong); }
  .team-card-heading strong, .team-card-heading span { display: block; }
  .team-card-heading strong { font-size: 11px; font-weight: 650; }
  .team-card-heading div > span { margin-top: 3px; color: var(--muted); font-size: 7px; letter-spacing: 0.08em; }
  .team-card :global(.markdown) { margin: 9px 0; color: var(--ink-soft); font-size: 14px; line-height: 1.55; }
  .team-card button { display: inline-flex; align-items: center; gap: 5px; padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: 9px; font-weight: 650; }
  .review-card { background: #f4f2ed; }
  .review-risk { display: inline-block; margin-top: 4px; padding: 2px 6px; border-radius: 5px; background: #f4e4e1; color: var(--danger); font-size: 7px; font-weight: 650; letter-spacing: 0.04em; }
  .review-audit { margin: 0 0 9px; padding: 6px 9px; border-radius: 8px; background: var(--surface); color: var(--muted); font-size: 9px; line-height: 1.45; }
  .review-audit[data-audited="true"] { color: var(--accent-strong); }
  .review-audit[data-audited="false"], .review-audit[data-audited="legacy"] { color: var(--warning); }
  .lead-notice { margin: 0 0 12px; padding: 12px 14px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
  .lead-notice p { margin: 0; color: var(--ink-soft); font-size: 12px; line-height: 1.5; }
  .lead-notice .notice-detail { margin-top: 5px; color: var(--muted); font-size: 10px; }
  .lead-error { border-color: #e3c7c2; background: #fbf3f1; }
  .lead-error p { color: var(--danger); }
  .lead-pending { border-color: #ddd2bd; background: #f8f4ea; }
  .notice-label { margin-bottom: 5px; color: var(--muted); font-size: 8px; font-weight: 650; letter-spacing: 0.08em; }
  .notice-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 10px; }
  .composer-dock { flex: 0 0 auto; padding: 10px 32px 12px; background: linear-gradient(transparent, var(--canvas) 18%); }
  @media (max-width: 760px) {
    .lead-content { padding: 16px; }
    .composer-dock { padding: 10px 16px 72px; }
  }
  @media (max-width: 560px) {
    .chat-entry:not(.user-entry), .tool-activity-row, .team-card { margin-left: 0; }
  }
</style>
