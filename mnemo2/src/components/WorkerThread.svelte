<script lang="ts">
  import { Send } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { actions } from "../lib/store";
  import type { Worker } from "../lib/types";
  import Collapsible from "./Collapsible.svelte";
  import Markdown from "./Markdown.svelte";
  import ToolActivity from "./ToolActivity.svelte";

  /**
   * A worker's live conversation — streamed messages and tool calls, screenshot
   * links, and the private reply box. In `compact` mode (the lead-side worker
   * panel) the messages scroll on their own and follow the stream while the
   * reader stays near the bottom; scrolling up to read pauses the follow.
   */
  let { worker, compact = false }: { worker: Worker; compact?: boolean } = $props();

  /** The reader counts as "at the bottom" within this many pixels (same rule as LeadChat). */
  const STICK_TO_BOTTOM_PX = 48;

  let reply = $state("");
  let lastWorkerId = "";
  let scroller = $state<HTMLDivElement | null>(null);
  let stream = $state<HTMLDivElement | null>(null);
  let nearBottom = true;
  let scrollFrame: number | null = null;
  let resizeObserver: ResizeObserver | null = null;

  $effect(() => {
    const id = worker.id;
    if (id === lastWorkerId) return;
    lastWorkerId = id;
    reply = "";
    nearBottom = true;
  });

  onMount(() => {
    if (!compact) return;
    if (typeof ResizeObserver !== "undefined") {
      resizeObserver = new ResizeObserver(() => scheduleScroll());
      if (stream) resizeObserver.observe(stream);
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

  // Follow the stream while the reader is near the bottom.
  $effect(() => {
    if (!compact) return;
    worker.messages.length;
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

{#snippet threadMessages()}
  {#each worker.messages as entry (entry.id)}
    {#if entry.kind === "tool"}
      <div class="thread-tool"><ToolActivity {entry} /></div>
    {:else}
      <div class="thread-message" class:from-lead={entry.from === "lead"} class:from-harness={entry.from === "harness"}>
        <span>{entry.from === "lead" ? "LEAD" : entry.from === "harness" ? "HARNESS NOTE" : worker.name.toUpperCase()} · {entry.time}</span>
        <div class="thread-message-body"><Collapsible maxHeight={160} label="message"><Markdown source={entry.body} onOpenLink={actions.openBrowser} /></Collapsible></div>
      </div>
    {/if}
  {/each}
{/snippet}

<section class="thread-panel" class:compact aria-label={`${worker.name} worker conversation`}>
  {#if compact}
    <div class="thread-messages" bind:this={scroller} onscroll={handleScroll}>
      <div class="thread-stream" bind:this={stream}>{@render threadMessages()}</div>
    </div>
  {:else}
    <div class="thread-messages">{@render threadMessages()}</div>
  {/if}
  {#if worker.screenshots.length > 0}
    <div class="screenshot-links" aria-label="Worker screenshots">
      <span>SCREENSHOTS</span>
      <div>
        {#each worker.screenshots as path (path)}
          <button class="button button-quiet" title={`Open ${path}`} onclick={() => actions.openScreenshot(worker.id, path)}>{path.split("/").pop()}</button>
        {/each}
      </div>
    </div>
  {/if}
  <div class="thread-composer">
    <textarea bind:value={reply} onkeydown={replyKeydown} rows="2" aria-label={`Reply to ${worker.name}`} placeholder={`Message ${worker.name}`} disabled={worker.status === "cancelled"}></textarea>
    <button class="icon-button" aria-label={`Send reply to ${worker.name}`} disabled={!reply.trim() || worker.status === "cancelled"} onclick={sendReply}><Send size={14} /></button>
  </div>
</section>

<style>
  .thread-message > span { color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.07em; }
  .thread-message-body { margin: 5px 0 0; padding: 10px 12px; border-radius: 4px 12px 12px 12px; background: var(--surface-soft); color: var(--ink-soft); }
  .thread-message-body :global(.markdown) { font-size: var(--text-body); line-height: 1.5; }
  .thread-message-body :global(p) { margin-bottom: 8px; }
  .thread-message-body :global(.markdown h1), .thread-message-body :global(.markdown h2), .thread-message-body :global(.markdown h3) { font-size: var(--text-body); }
  .thread-message-body :global(.markdown h4), .thread-message-body :global(.markdown h5), .thread-message-body :global(.markdown h6) { font-size: var(--text-body); }
  .thread-message.from-lead { margin-left: auto; }
  .thread-message.from-lead > span { display: block; text-align: right; }
  .thread-message.from-lead .thread-message-body { border-radius: 12px 12px 4px 12px; background: var(--accent-soft); }
  .thread-message.from-harness .thread-message-body { border: 1px dashed var(--line-strong); background: transparent; color: var(--muted); }
  .thread-message + .thread-message, .thread-tool + .thread-message { margin-top: 12px; }
  .thread-tool { min-width: 0; margin: 4px 0; }
  .screenshot-links { display: grid; gap: 7px; margin-top: 16px; }
  .screenshot-links > span { color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.08em; }
  .screenshot-links > div { display: flex; flex-wrap: wrap; gap: 6px; }
  .thread-composer { display: flex; align-items: flex-end; gap: 8px; margin-top: 16px; padding: 9px; border: 1px solid var(--line-strong); border-radius: 12px; background: var(--surface); }
  .thread-composer textarea { flex: 1; resize: none; border: 0; outline: 0; color: var(--ink); font-size: var(--text-body); }

  /* Compact: the panel body scrolls the messages alone so the reply box stays put. */
  .compact { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  .compact .thread-messages { flex: 1; overflow-y: auto; min-height: 0; }
  .compact .thread-composer { flex: 0 0 auto; margin-top: 10px; }
  .thread-stream { min-width: 0; }
</style>