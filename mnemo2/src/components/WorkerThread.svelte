<script module lang="ts">
  /**
   * Half-typed replies, kept per worker id: switching tabs or workers unmounts
   * the thread, and the draft must survive that instead of dying with it.
   */
  const drafts = new Map<string, string>();
</script>

<script lang="ts">
  import { onMount } from "svelte";
  import { actions } from "../lib/store";
  import { turnStartTimes } from "../lib/elapsed";
  import { earlierEntriesLabel, ENTRY_WINDOW, windowEntries } from "../lib/entryWindow";
  import { createStickToBottom } from "../lib/stickToBottom";
  import { workerBox } from "../lib/workerComposer";
  import { workerPhaseLabel } from "../lib/workerActivity";
  import type { Worker } from "../lib/types";
  import ActivityStatus from "./ActivityStatus.svelte";
  import Collapsible from "./Collapsible.svelte";
  import ComposerBox from "./ComposerBox.svelte";
  import Markdown from "./Markdown.svelte";
  import ToolActivity from "./ToolActivity.svelte";

  /**
   * A worker's live conversation — streamed messages and tool calls, screenshot
   * links, and the shared message box. Every view scrolls the messages on their
   * own and follows the stream while the reader stays near the bottom; scrolling
   * up to read pauses the follow, and the box stays pinned below. `compact` (the
   * lead-side worker panel) only drops the duplicate failure block and tightens
   * the composer's margin.
   */
  let { worker, compact = false }: { worker: Worker; compact?: boolean } = $props();

  /** Each tool call's turn start: its card shows how long the turn has run. */
  const turnStarts = $derived(turnStartTimes(worker.messages));

  // A long thread keeps every message in state but only its newest window in
  // the DOM; the button above the list grows the window on demand.
  let entryLimit = $state(ENTRY_WINDOW);
  const windowed = $derived(windowEntries(worker.messages, entryLimit));

  /** What this worker's box offers now: steer it, message it, or why it can't. */
  const box = $derived(workerBox(worker));

  let reply = $state("");
  let sending = $state(false);
  let lastWorkerId = "";
  let scroller = $state<HTMLDivElement | null>(null);
  let stream = $state<HTMLDivElement | null>(null);
  const follow = createStickToBottom(() => scroller);

  $effect(() => {
    const id = worker.id;
    if (id === lastWorkerId) return;
    lastWorkerId = id;
    reply = drafts.get(id) ?? "";
    entryLimit = ENTRY_WINDOW;
    follow.reset();
  });

  onMount(() => {
    follow.schedule();
    return () => follow.dispose();
  });

  // Follow the stream while the reader is near the bottom; the live status
  // row ends the stream, so its changes count as stream growth too.
  $effect(() => {
    worker.messages.length;
    worker.activity?.phase;
    follow.schedule();
  });

  // Content that grows between frames (streamed markdown, a tool card) resizes
  // the stream; follow it while the reader is still at the bottom.
  $effect(() => {
    if (stream) return follow.observe(stream);
  });

  function setReply(text: string) {
    reply = text;
    if (text) drafts.set(worker.id, text);
    else drafts.delete(worker.id);
  }

  async function sendReply() {
    const text = reply;
    const id = worker.id;
    if (!box.canSend || sending || !text.trim()) return;
    sending = true;
    try {
      // A refused message keeps its text: the box clears only once accepted.
      if (!(await actions.replyWorker(id, text))) return;
      if (drafts.get(id) === text) drafts.delete(id);
      if (id === worker.id && reply === text) reply = "";
    } finally {
      sending = false;
    }
  }
</script>

{#snippet threadMessages()}
  {#if windowed.hidden > 0}
    <button type="button" class="earlier-entries" onclick={() => (entryLimit += ENTRY_WINDOW)}>{earlierEntriesLabel(windowed.hidden)}</button>
  {/if}
  {#each windowed.visible as entry (entry.id)}
    {#if entry.kind === "tool"}
      <div class="thread-tool"><ToolActivity {entry} turnStartedAtMs={turnStarts[entry.id]} workerId={worker.id} /></div>
    {:else if entry.kind === "error"}
      {#if entry.retrying}
        <div class="thread-error-note" role="status">Provider error, retrying: {entry.body}</div>
      {:else}
        <div class="thread-error" role="alert">
          <span>WORKER ERROR</span>
          <p>{entry.body}</p>
        </div>
      {/if}
    {:else if entry.kind === "model_switch"}
      <div class="thread-model-note" role="status">{entry.body}</div>
    {:else}
      <div class="thread-message" class:from-lead={entry.from === "lead"} class:from-harness={entry.from === "harness"}>
        <span>{entry.from === "lead" ? "LEAD" : entry.from === "harness" ? "HARNESS NOTE" : worker.name.toUpperCase()} · {entry.time}</span>
        <div class="thread-message-body"><Collapsible maxHeight={160} label="message"><Markdown source={entry.body} onOpenLink={actions.openBrowser} /></Collapsible></div>
      </div>
    {/if}
  {/each}
  {#if worker.activity}
    <div class="thread-activity">
      <ActivityStatus
        label={workerPhaseLabel(worker.activity)}
        startedAtMs={worker.activity.turnStartedAtMs}
        pulse={worker.activity.phase !== "waiting"}
      />
    </div>
  {/if}
  <!-- The compact panel already carries the error in its blocked banner with its
       way-out actions; only mission control needs the thread's failure block. -->
  {#if !compact && worker.status === "failed" && worker.error}
    <article class="thread-error thread-failure" role="alert">
      <span>WORKER FAILED</span>
      <p>{worker.error}</p>
      <p class="failure-detail">The lead can resume this worker on another model.</p>
    </article>
  {/if}
{/snippet}

<section class="thread-panel" class:compact aria-label={`${worker.name} worker conversation`}>
  <div class="thread-messages" bind:this={scroller} onscroll={follow.onScroll}>
    <div class="thread-stream" bind:this={stream}>{@render threadMessages()}</div>
  </div>
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
  <form
    class="thread-composer"
    onsubmit={(event) => {
      event.preventDefault();
      void sendReply();
    }}
  >
    <ComposerBox
      value={box.mode === "done" ? "" : reply}
      label={`Reply to ${worker.name}`}
      placeholder={box.placeholder}
      disabled={!box.canSend}
      send={{ ariaLabel: box.sendLabel, enabled: box.canSend && !sending && reply.trim() !== "" }}
      stop={box.canPause
        ? {
            ariaLabel: `Pause ${worker.name}`,
            text: "Pause",
            busy: false,
            onclick: () => actions.pauseWorker(worker.id),
          }
        : null}
      oninput={setReply}
      onsubmit={() => void sendReply()}
    />
  </form>
</section>

<style>
  .earlier-entries { display: block; margin: 0 auto 12px; padding: 3px 10px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--muted); cursor: pointer; font: inherit; font-size: var(--text-meta); }
  .earlier-entries:hover { border-color: var(--line-strong); color: var(--ink-soft); }
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
  .thread-error { margin-top: 12px; padding: 12px 14px; border: 1px solid var(--danger-tint-line); border-radius: 12px; background: var(--danger-tint); }
  .thread-error > span { display: block; margin-bottom: 5px; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.08em; }
  .thread-error p { margin: 0; color: var(--danger); font-size: var(--text-body); line-height: 1.5; white-space: pre-wrap; overflow-wrap: anywhere; }
  .thread-error .failure-detail { margin-top: 5px; color: var(--muted); font-size: var(--text-body); }
  .thread-error-note { margin-top: 12px; padding: 8px 10px; border-radius: 4px 12px 12px 12px; background: var(--surface-soft); color: var(--muted); font-size: var(--text-body); }
  .thread-model-note { margin-top: 12px; padding: 8px 10px; border: 1px dashed var(--line-strong); border-radius: 4px 12px 12px 12px; color: var(--muted); font-size: var(--text-body); }
  .thread-activity { margin-top: 12px; }
  .screenshot-links { display: grid; flex: 0 0 auto; gap: 7px; margin-top: 16px; }
  .screenshot-links > span { color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.08em; }
  .screenshot-links > div { display: flex; flex-wrap: wrap; gap: 6px; }
  .thread-composer { flex: 0 0 auto; margin-top: 16px; }

  /* The messages scroll on their own and the box stays pinned below, in every view. */
  .thread-panel { display: flex; flex: 1 1 auto; flex-direction: column; min-width: 0; min-height: 0; }
  .thread-messages { flex: 1 1 0; min-height: 0; overflow-y: auto; }
  /* Compact: the panel body already gives the height and has no padding of its
     own, so the box keeps its distance from the panel's edges here. */
  .compact { height: 100%; }
  .compact .thread-composer { margin: 10px 12px 12px; }
  .thread-stream { min-width: 0; }
</style>