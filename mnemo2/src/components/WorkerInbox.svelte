<script lang="ts">
  import { formatAgo } from "../lib/elapsed";
  import { actions } from "../lib/store";
  import type { InboxKind, InboxSender, Worker } from "../lib/types";
  import Collapsible from "./Collapsible.svelte";
  import Markdown from "./Markdown.svelte";

  /**
   * The durable ledger of every message this worker received — who sent it,
   * in what form, and when — so a reader never has to trust a worker's own
   * account of its instructions. The host stores the entries oldest first.
   *
   * The list is collapsed by default like the worker brief: the first entries
   * peek out and the rest stay behind the toggle; each entry's Markdown body
   * clamps on its own, like a thread message.
   */
  let { worker }: { worker: Worker } = $props();

  /** Mission control's name for each sender. */
  const SENDER_LABEL: Record<InboxSender, string> = {
    external_lead: "Claude Code",
    lead: "Built-in lead",
    user: "You",
    mnemo: "Mnemo2",
  };
  /** Prompt and brief are the norm; only the interrupting forms get a tag. */
  const KIND_TAG: Partial<Record<InboxKind, string>> = { steer: "steer", fix: "fix" };
</script>

{#if worker.inbox.length > 0}
  <section class="inbox-panel" aria-label={`Messages received by ${worker.name}`}>
    <div class="inbox-heading">RECEIVED MESSAGES · {worker.inbox.length}</div>
    <Collapsible maxHeight={190} label="received messages">
      <div class="inbox-entries">
        {#each worker.inbox as entry, index (index)}
          {@const tag = KIND_TAG[entry.kind]}
          <article class="inbox-entry">
            <div class="inbox-meta">
              <strong>{SENDER_LABEL[entry.sender]}</strong>
              {#if tag}<span class="kind-tag" data-kind={entry.kind}>{tag}</span>{/if}
              <time>{formatAgo(entry.atMs)}</time>
            </div>
            <div class="inbox-body">
              <Collapsible maxHeight={160} label="received message">
                <Markdown source={entry.text} onOpenLink={actions.openBrowser} />
              </Collapsible>
            </div>
          </article>
        {/each}
      </div>
    </Collapsible>
  </section>
{/if}

<style>
  .inbox-panel { margin: 0 0 18px; }
  .inbox-heading { margin-bottom: 8px; color: var(--muted); font-size: 8px; letter-spacing: 0.08em; }
  .inbox-entry { min-width: 0; }
  .inbox-entry + .inbox-entry { margin-top: 10px; }
  .inbox-meta { display: flex; align-items: center; gap: 6px; }
  .inbox-meta strong { font-size: 9px; font-weight: 650; }
  .kind-tag { padding: 2px 5px; border-radius: 5px; background: var(--surface-soft); color: var(--ink-soft); font-size: 7px; font-weight: 650; text-transform: uppercase; }
  .inbox-meta time { margin-left: auto; color: var(--muted); font-size: 8px; }
  .inbox-body { margin: 4px 0 0; padding: 9px 11px; border-radius: 4px 12px 12px 12px; background: var(--surface-soft); }
  .inbox-body :global(.markdown) { font-size: 11px; line-height: 1.5; }
  .inbox-body :global(p) { margin-bottom: 8px; }
</style>