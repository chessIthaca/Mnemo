<script lang="ts">
  import { onMount } from "svelte";
  import { ChevronDown, ChevronRight, ChevronUp, Clock, ListTodo, Pencil, Plus, Trash2 } from "@lucide/svelte";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import {
    createBacklogTransport,
    finishedItems,
    ideaTitle,
    inFlightItems,
    isDeferred,
    movePending,
    pendingItems,
    type BacklogItem,
    type BacklogTransport,
  } from "../lib/backlog";

  /**
   * The I/O seam. Defaults to the native transport (or the preview seed in a
   * plain browser); tests and the preview page can pass a stand-in.
   */
  let { transport: provided = null }: { transport?: BacklogTransport | null } = $props();

  const transport = $derived(provided ?? createBacklogTransport());

  let loading = $state(true);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let items = $state<BacklogItem[]>([]);
  let draft = $state("");
  let editingId = $state<string | null>(null);
  let editingText = $state("");
  let confirmingId = $state<string | null>(null);
  /** The idea title of the most recent Plan-this handoff — the section's
   * confirmation that it reached the lead. */
  let sent = $state<string | null>(null);
  /** Which rows are expanded to their full text and actions, by item id. */
  let open = $state<Record<string, boolean>>({});
  /** Whether the finished rows under "Done" are showing. */
  let doneOpen = $state(false);

  /** The pending ideas, in the store's order. */
  const queue = $derived(pendingItems(items));
  /** The ideas the lead is working on right now. */
  const flight = $derived(inFlightItems(items));
  /** The finished ideas (done, failed, can't resolve), in the store's order. */
  const finishedList = $derived(finishedItems(items));

  /** One finished row's status word, spelled for the badge. */
  function statusLabel(item: BacklogItem): string {
    return item.status === "cant_resolve" ? "Can't resolve" : item.status;
  }

  /** Show or hide one row's full text and actions. */
  function toggleRow(id: string): void {
    open[id] = open[id] !== true;
  }

  onMount(() => {
    const active = transport;
    if (!active) {
      loading = false;
      return;
    }
    let stopped = false;
    let unlisten: UnlistenFn | null = null;

    const refresh = async () => {
      try {
        const next = await active.items();
        if (!stopped) items = next;
      } catch (cause) {
        if (!stopped) error = errorText(cause);
      } finally {
        if (!stopped) loading = false;
      }
    };
    void refresh();
    void active
      .onChanged(() => void refresh())
      .then((stop) => {
        if (stopped) stop();
        else unlisten = stop;
      })
      .catch((cause: unknown) => {
        if (!stopped) error = errorText(cause);
      });
    return () => {
      stopped = true;
      unlisten?.();
    };
  });

  function errorText(cause: unknown): string {
    if (typeof cause === "string") return cause;
    if (cause instanceof Error) return cause.message;
    return "the backlog write failed";
  }

  /** Run one write, then adopt the store's full list as the new state. */
  async function mutate(action: () => Promise<BacklogItem[]>): Promise<void> {
    if (!transport) return;
    busy = true;
    error = null;
    try {
      items = await action();
    } catch (cause) {
      error = errorText(cause);
    } finally {
      busy = false;
    }
  }

  function add(): void {
    if (!transport) return;
    const text = draft.trim();
    if (text === "") return;
    void mutate(async () => {
      const next = await transport.add(text);
      draft = "";
      return next;
    });
  }

  function startEdit(item: BacklogItem): void {
    editingId = item.id;
    editingText = item.text;
    confirmingId = null;
  }

  function cancelEdit(): void {
    editingId = null;
    editingText = "";
  }

  function saveEdit(): void {
    const id = editingId;
    if (id === null || !transport || editingText.trim() === "") return;
    const text = editingText;
    void mutate(async () => {
      const next = await transport.edit(id, text);
      editingId = null;
      editingText = "";
      return next;
    });
  }

  function remove(item: BacklogItem): void {
    if (!transport) return;
    const id = item.id;
    confirmingId = null;
    void mutate(() => transport.remove(id));
  }

  function defer(item: BacklogItem): void {
    if (!transport) return;
    void mutate(() => transport.setDeferred(item.id, !isDeferred(item)));
  }

  /** Check an idea off (pending -> done) or put a finished one back in the queue. */
  function setDone(item: BacklogItem, done: boolean): void {
    if (!transport) return;
    void mutate(() => transport.setDone(item.id, done));
  }

  function move(item: BacklogItem, delta: number): void {
    if (!transport) return;
    const order = movePending(items, item.id, delta);
    if (order === null) return;
    void mutate(() => transport.reorder(order));
  }

  function clearFinished(): void {
    if (!transport) return;
    void mutate(() => transport.clearFinished());
  }

  /** Hand one idea to the lead to plan ("Plan this"): the store marks it
   * in_flight with a "handed to the lead" note and the lead receives the
   * notice — queued for an external lead, else a user message to the
   * in-app lead. The section confirms which idea was sent. */
  function planThis(item: BacklogItem): void {
    if (!transport) return;
    const id = item.id;
    const title = ideaTitle(item);
    void mutate(async () => {
      const next = await transport.planThis(id);
      sent = title;
      return next;
    });
  }
</script>

{#snippet row(item: BacklogItem, index: number, group: "queue" | "flight" | "done")}
  {@const isOpen = open[item.id] === true}
  {@const checked = group === "done"}
  <li class="item" data-deferred={isDeferred(item)} data-status={item.status} data-open={isOpen}>
    <div class="row-head">
      <input
        type="checkbox"
        class="check"
        checked={checked}
        disabled={busy || group === "flight"}
        aria-label={`${checked ? "Mark not done" : "Mark done"} ${ideaTitle(item)}`}
        onchange={() => setDone(item, group !== "done")}
      />
      <button
        class="expand"
        type="button"
        aria-expanded={isOpen}
        aria-label={`Expand ${ideaTitle(item)}`}
        onclick={() => toggleRow(item.id)}
      ><ChevronRight size={11} /></button>
      <button class="row-title" type="button" onclick={() => toggleRow(item.id)}
        >{ideaTitle(item, 90)}</button
      >
      {#if isDeferred(item)}<span class="badge">Deferred</span>{/if}
      {#if group === "flight"}<span class="badge">With the lead</span>{/if}
      {#if group === "done" && item.status !== "done"}<span class="badge">{statusLabel(item)}</span>{/if}
    </div>
    {#if isOpen}
      <div class="row-body">
        {#if group === "queue" && editingId === item.id}
          <textarea
            class="edit-box"
            bind:value={editingText}
            aria-label={`Edit text of ${ideaTitle(item)}`}
            rows="3"
          ></textarea>
          <div class="actions">
            <button
              class="action primary"
              type="button"
              disabled={busy || editingText.trim() === ""}
              onclick={saveEdit}>Save</button
            >
            <button class="action" type="button" onclick={cancelEdit}>Cancel</button>
          </div>
        {:else if group === "queue"}
          <p class="text">{item.text}</p>
          <div class="actions">
            <button
              class="action"
              type="button"
              aria-label={`Plan this ${ideaTitle(item)}`}
              disabled={busy || undefined}
              onclick={() => planThis(item)}><ListTodo size={11} /> Plan this</button
            >
            <button
              class="action"
              type="button"
              aria-label={`Edit ${ideaTitle(item)}`}
              onclick={() => startEdit(item)}><Pencil size={11} /> Edit</button
            >
            <button
              class="action"
              type="button"
              aria-label={`Move ${ideaTitle(item)} up`}
              disabled={busy || index === 0}
              onclick={() => move(item, -1)}><ChevronUp size={11} /></button
            >
            <button
              class="action"
              type="button"
              aria-label={`Move ${ideaTitle(item)} down`}
              disabled={busy || index === queue.length - 1}
              onclick={() => move(item, 1)}><ChevronDown size={11} /></button
            >
            <button
              class="action"
              type="button"
              aria-label={`${isDeferred(item) ? "Undefer" : "Defer"} ${ideaTitle(item)}`}
              disabled={busy || undefined}
              onclick={() => defer(item)}><Clock size={11} /> {isDeferred(item) ? "Undefer" : "Defer"}</button
            >
            {#if confirmingId === item.id}
              <button
                class="action danger"
                type="button"
                aria-label={`Confirm remove ${ideaTitle(item)}`}
                disabled={busy || undefined}
                onclick={() => remove(item)}>Remove</button
              >
              <button class="action" type="button" onclick={() => (confirmingId = null)}
                >Keep</button
              >
            {:else}
              <button
                class="action"
                type="button"
                aria-label={`Remove ${ideaTitle(item)}`}
                onclick={() => (confirmingId = item.id)}><Trash2 size={11} /> Remove</button
              >
            {/if}
          </div>
        {:else}
          <p class="text">{item.text}</p>
          {#if item.note}<p class="row-note">{item.note}</p>{/if}
        {/if}
      </div>
    {/if}
  </li>
{/snippet}

<section class="backlog" aria-label="Backlog">
  <header class="backlog-header">
    <h2 class="backlog-title">Backlog <span class="count">{queue.length}</span></h2>
    <p>Ideas that are not plans yet — the queue the next plan is written from.</p>
  </header>

  <div class="add">
    <textarea
      bind:value={draft}
      aria-label="New backlog item"
      placeholder="An idea worth planning…"
      rows="2"
      onkeydown={(event) => {
        if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
          event.preventDefault();
          add();
        }
      }}
    ></textarea>
    <button
      class="action primary"
      type="button"
      disabled={busy || draft.trim() === ""}
      onclick={add}><Plus size={11} /> Add</button
    >
  </div>

  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if sent !== null}<p class="note">Sent to the lead — {sent}</p>{/if}

  {#if loading}
    <p class="note">Loading backlog…</p>
  {:else if transport === null}
    <p class="note">The backlog is a project file — open the Mnemo2 desktop app to edit it.</p>
  {:else}
    {#if queue.length === 0}
      <p class="note">No ideas yet. Add the first one above.</p>
    {:else}
      <ol class="items">
        {#each queue as item, index (item.id)}{@render row(item, index, "queue")}{/each}
      </ol>
    {/if}

    {#if flight.length > 0}
      <h3 class="group">With the lead <span class="count">{flight.length}</span></h3>
      <ol class="items">
        {#each flight as item, index (item.id)}{@render row(item, index, "flight")}{/each}
      </ol>
    {/if}

    {#if finishedList.length > 0}
      <button
        class="group group-toggle"
        type="button"
        aria-expanded={doneOpen}
        onclick={() => (doneOpen = !doneOpen)}
      ><ChevronRight size={11} /> Done <span class="count">{finishedList.length}</span></button>
      {#if doneOpen}
        <ol class="items">
          {#each finishedList as item, index (item.id)}{@render row(item, index, "done")}{/each}
        </ol>
        <button
          class="action clear-finished"
          type="button"
          disabled={busy || undefined}
          onclick={clearFinished}>Clear finished</button
        >
      {/if}
    {/if}
  {/if}
</section>

<style>
  .backlog-header { display: flex; flex-direction: column; gap: 4px; }
  .backlog-title { display: flex; align-items: center; gap: 6px; margin: 0; color: var(--ink); font-size: var(--text-title); font-weight: 600; }
  .count { padding: 0 5px; border-radius: 999px; background: var(--surface-soft); color: var(--muted); font-size: var(--text-meta); font-weight: 500; font-variant-numeric: tabular-nums; letter-spacing: 0; }
  .backlog-header p { margin: 0; color: var(--muted); font-size: var(--text-body); }
  .add { display: flex; align-items: flex-end; gap: 8px; margin-top: 12px; }
  .add textarea, .edit-box { flex: 1; min-width: 0; padding: 8px 10px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink); font: inherit; font-size: var(--text-body); line-height: 1.45; resize: vertical; }
  .add textarea:focus-visible, .edit-box:focus-visible { outline: 2px solid var(--accent-strong); outline-offset: 1px; }
  .items { margin: 10px 0 0; padding: 0; list-style: none; }
  .item { border-top: 1px solid var(--line); }
  .row-head { display: flex; align-items: center; gap: 6px; padding: 7px 0; }
  .check { flex: 0 0 auto; width: 14px; height: 14px; margin: 0; accent-color: var(--accent-strong); cursor: pointer; }
  .check:disabled { opacity: 0.45; cursor: default; }
  .expand { display: grid; flex: 0 0 auto; width: 14px; height: 14px; padding: 0; place-items: center; border: 0; border-radius: 4px; background: transparent; color: var(--muted); cursor: pointer; }
  .expand:hover { color: var(--ink-soft); }
  .expand :global(svg) { transition: transform 120ms ease; }
  .item[data-open="true"] .expand :global(svg) { transform: rotate(90deg); }
  .row-title { flex: 1; min-width: 0; padding: 0; overflow: hidden; border: 0; background: transparent; color: var(--ink); font: inherit; font-size: var(--text-body); line-height: 1.45; text-align: left; text-overflow: ellipsis; white-space: nowrap; cursor: pointer; }
  .row-title:hover { color: var(--accent-strong); }
  .item[data-status="done"] .row-title, .item[data-status="failed"] .row-title, .item[data-status="cant_resolve"] .row-title { color: var(--muted); text-decoration: line-through; }
  .expand:focus-visible, .row-title:focus-visible, .group-toggle:focus-visible { outline: 2px solid var(--accent-strong); outline-offset: 1px; }
  .row-body { padding: 0 0 10px 40px; }
  .row-note { margin: 6px 0 0; color: var(--muted); font-size: var(--text-meta); }
  .item[data-deferred="true"] .text { color: var(--muted); }
  .text { margin: 0; white-space: pre-wrap; font-size: var(--text-body); line-height: 1.5; }
  .actions { display: flex; flex-wrap: wrap; align-items: center; gap: 5px; margin-top: 6px; }
  .action { display: inline-flex; align-items: center; gap: 3px; padding: 2px 7px; border: 1px solid var(--line); border-radius: 6px; background: transparent; color: var(--muted); font-size: var(--text-meta); cursor: pointer; }
  .action:hover:enabled { background: var(--surface-soft); color: var(--ink-soft); }
  .action:focus-visible { outline: 2px solid var(--accent-strong); outline-offset: 1px; }
  .action:disabled { opacity: 0.45; cursor: default; }
  .action.primary { border-color: var(--accent-strong); background: var(--accent-soft); color: var(--accent-strong); }
  .action.danger { border-color: var(--danger); color: var(--danger); }
  .badge { padding: 1px 6px; border-radius: 999px; background: var(--surface-soft); color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.06em; text-transform: uppercase; }
  .group { display: flex; align-items: center; gap: 5px; margin: 18px 0 0; color: var(--muted); font-size: var(--text-meta); font-weight: 600; letter-spacing: 0.08em; text-transform: uppercase; }
  .group-toggle { padding: 0; border: 0; background: transparent; cursor: pointer; }
  .group-toggle:hover { color: var(--ink-soft); }
  .group-toggle :global(svg) { transition: transform 120ms ease; }
  .group-toggle[aria-expanded="true"] :global(svg) { transform: rotate(90deg); }
  .clear-finished { margin-top: 8px; }
  .note { margin: 10px 0 0; color: var(--muted); font-size: var(--text-body); }
  .error { margin: 8px 0 0; color: var(--danger); font-size: var(--text-body); }
</style>
