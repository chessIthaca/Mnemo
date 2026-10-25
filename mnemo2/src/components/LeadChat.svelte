<script lang="ts">
  import { onMount } from "svelte";
  import { ArrowUpRight, ClipboardCheck, MessageSquareText } from "@lucide/svelte";
  import { actions, app, currentProject } from "../lib/store";
  import Composer from "./Composer.svelte";
  import Markdown from "./Markdown.svelte";
  import ModelBadge from "./ModelBadge.svelte";
  import MergeCard from "./MergeCard.svelte";
  import ToolActivity from "./ToolActivity.svelte";

  const STICK_TO_BOTTOM_PX = 48;

  let scroller: HTMLDivElement;
  let content: HTMLDivElement;
  let nearBottom = true;
  let scrollFrame: number | null = null;
  let resizeObserver: ResizeObserver | null = null;

  const leadModel = $derived(
    $app.leadRuntime.modelId ?? $currentProject?.leadModel ?? "unconfigured",
  );

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
      <header class="lead-heading">
        <div class="lead-title"><span class="lead-mark">M</span><h1>Lead</h1><ModelBadge model={leadModel} role="lead" /></div>
      </header>

      <div class="lead-stream" bind:this={content}>
      {#if $app.leadRuntime.status === "error" && $app.leadRuntime.error}
        <article class="lead-notice lead-error" role="alert">
          <p>{$app.leadRuntime.error}</p>
          <div class="notice-actions">
            <button class="button button-quiet" onclick={() => actions.retryLead()}>Retry</button>
          </div>
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

      <div class="lead-entries">
        {#each $app.leadEntries as entry (entry.id)}
          {#if entry.kind === "message"}
            <article class:user-entry={entry.author === "user"} class="chat-entry">
              {#if entry.author === "lead"}
                <div class="entry-label"><span class="lead-mark">M</span> LEAD <time>{entry.time}</time></div>
                <Markdown source={entry.body} onOpenLink={actions.openBrowser} />
              {:else}
                <p>{entry.body}</p>
                <div class="user-meta">YOU · {entry.time}</div>
              {/if}
            </article>
          {:else if entry.kind === "tool"}
            <ToolActivity {entry} />
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
            <article class="team-card review-card">
              <div class="team-card-heading">
                <span class="team-card-icon"><ClipboardCheck size={15} /></span>
                <div><strong>{entry.title}</strong><span>LEAD REVIEW · {entry.time}</span></div>
              </div>
              <Markdown source={entry.body} onOpenLink={actions.openBrowser} />
              <button onclick={() => actions.selectWorker(entry.workerId)}>Review {worker?.name ?? "worker"}’s delta <ArrowUpRight size={13} /></button>
            </article>
          {:else if entry.kind === "merge"}
            <MergeCard {entry} />
          {/if}
        {/each}
      </div>
      </div>
    </div>
  </div>
  <footer class="composer-dock"><Composer /></footer>
</section>

<style>
  .lead-workspace { display: flex; min-width: 0; min-height: 0; flex: 1; flex-direction: column; background: var(--canvas); }
  .lead-scroll { flex: 1; min-width: 0; min-height: 0; overflow-y: auto; scrollbar-gutter: stable; }
  .lead-content { width: 100%; min-width: 0; margin: 0; padding: 28px 40px 24px; }
  .lead-stream { display: flow-root; min-width: 0; }
  .lead-heading { display: flex; align-items: center; gap: 12px; margin-bottom: 18px; }
  .lead-title { display: flex; align-items: center; gap: 9px; }
  .lead-heading h1 { margin: 0; font-family: var(--font-prose); font-size: 30px; font-weight: 500; letter-spacing: -0.03em; }
  .lead-title :global(.model-badge) { margin-top: 3px; }
  .lead-entries { margin-top: 20px; }
  .chat-entry { margin: 0 0 20px; }
  .entry-label { display: flex; align-items: center; gap: 6px; margin-bottom: 9px; color: var(--muted); font-size: 8px; font-weight: 650; letter-spacing: 0.08em; }
  .entry-label time { margin-left: 2px; font-weight: 400; letter-spacing: 0; }
  .lead-mark { display: grid; width: 20px; height: 20px; place-items: center; border: 1px solid var(--line); border-radius: 6px; background: var(--surface); color: var(--accent-strong); font-family: var(--font-prose); font-size: 12px; }
  .user-entry { max-width: 620px; margin-left: auto; padding: 13px 16px 9px; border: 1px solid var(--line); border-radius: 16px 16px 4px 16px; background: var(--surface); }
  .user-entry p { margin: 0; color: var(--ink-soft); font-size: 12px; line-height: 1.6; white-space: pre-wrap; }
  .user-meta { margin-top: 6px; color: var(--muted); font-size: 7px; letter-spacing: 0.08em; text-align: right; }
  .team-card { margin: 14px 0 14px 30px; padding: 14px 16px; border: 1px solid var(--line); border-radius: 14px; background: var(--surface-soft); }
  .team-card-heading { display: flex; align-items: center; gap: 10px; }
  .team-card-icon { display: grid; width: 29px; height: 29px; place-items: center; border-radius: 9px; background: var(--surface); color: var(--accent-strong); }
  .team-card-heading strong, .team-card-heading span { display: block; }
  .team-card-heading strong { font-size: 11px; font-weight: 650; }
  .team-card-heading div > span { margin-top: 3px; color: var(--muted); font-size: 7px; letter-spacing: 0.08em; }
  .team-card :global(.markdown) { margin: 12px 0; color: var(--ink-soft); font-size: 14px; line-height: 1.55; }
  .team-card button { display: inline-flex; align-items: center; gap: 5px; padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: 9px; font-weight: 650; }
  .review-card { background: #f4f2ed; }
  .lead-notice { margin: 0 0 18px; padding: 12px 14px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
  .lead-notice p { margin: 0; color: var(--ink-soft); font-size: 12px; line-height: 1.5; }
  .lead-error { border-color: #e3c7c2; background: #fbf3f1; }
  .lead-error p { color: var(--danger); }
  .lead-pending { border-color: #ddd2bd; background: #f8f4ea; }
  .notice-label { margin-bottom: 5px; color: var(--muted); font-size: 8px; font-weight: 650; letter-spacing: 0.08em; }
  .notice-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 10px; }
  .composer-dock { flex: 0 0 auto; padding: 12px 40px 16px; background: linear-gradient(transparent, var(--canvas) 18%); }
  @media (max-width: 760px) {
    .lead-content { padding: 20px; }
    .composer-dock { padding: 10px 16px 72px; }
  }
  @media (max-width: 560px) {
    .lead-heading h1 { font-size: 27px; }
    .team-card { margin-left: 0; }
  }
</style>
