<script lang="ts">
  import { onMount } from "svelte";
  import { ChevronDown, ChevronUp, Clock, Pencil, Plus, Trash2 } from "@lucide/svelte";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import {
    createBacklogTransport,
    finishedCount,
    ideaTitle,
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

  let expanded = $state(true);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let items = $state<BacklogItem[]>([]);
  let draft = $state("");
  let editingId = $state<string | null>(null);
  let editingText = $state("");
  let confirmingId = $state<string | null>(null);

  /** The pending ideas, in the store's order. */
  const queue = $derived(pendingItems(items));
  /** How many finished ideas wait behind "Clear finished". */
  const finished = $derived(finishedCount(items));

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
</script>

<section class="backlog" aria-label="Backlog">
  <header class="backlog-header">
    <button
      class="backlog-toggle"
      type="button"
      aria-expanded={expanded}
      onclick={() => (expanded = !expanded)}
    >
      <ChevronDown size={11} />
      <span class="title">Backlog</span>
      <span class="count">{queue.length}</span>
    </button>
    <p>Ideas that are not plans yet — the queue the next plan is written from.</p>
  </header>

  {#if expanded}
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

    {#if loading}
      <p class="note">Loading backlog…</p>
    {:else if transport === null}
      <p class="note">The backlog is a project file — open the Mnemo2 desktop app to edit it.</p>
    {:else if queue.length === 0}
      <p class="note">No ideas yet. Add the first one above.</p>
    {:else}
      <ol class="items">
        {#each queue as item, index (item.id)}
          <li class="item" data-deferred={isDeferred(item)}>
            {#if editingId === item.id}
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
            {:else}
              <p class="text">{item.text}</p>
              <div class="actions">
                {#if isDeferred(item)}<span class="badge">Deferred</span>{/if}
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
            {/if}
          </li>
        {/each}
      </ol>
    {/if}

    {#if finished > 0}
      <footer class="finished">
        <span>{finished} finished</span>
        <button class="action" type="button" disabled={busy || undefined} onclick={clearFinished}
          >Clear finished</button
        >
      </footer>
    {/if}
  {/if}
</section>

<style>
  .backlog-header { display: flex; flex-direction: column; gap: 4px; }
  .backlog-toggle { display: inline-flex; align-items: center; gap: 5px; align-self: flex-start; padding: 2px 6px 2px 0; border: 1px solid transparent; border-radius: 6px; background: transparent; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.08em; text-transform: uppercase; cursor: pointer; }
  .backlog-toggle:hover { color: var(--ink-soft); }
  .backlog-toggle:focus-visible { outline: 2px solid var(--accent-strong); outline-offset: 1px; }
  .backlog-toggle :global(svg) { transition: transform 120ms ease; }
  .backlog-toggle[aria-expanded="false"] :global(svg) { transform: rotate(-90deg); }
  .backlog-toggle .title { font-weight: 620; }
  .count { padding: 0 5px; border-radius: 999px; background: var(--surface-soft); color: var(--muted); font-variant-numeric: tabular-nums; letter-spacing: 0; }
  .backlog-header p { margin: 0; color: var(--muted); font-size: var(--text-body); }
  .add { display: flex; align-items: flex-end; gap: 8px; margin-top: 12px; }
  .add textarea, .edit-box { flex: 1; min-width: 0; padding: 8px 10px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink); font: inherit; font-size: var(--text-body); line-height: 1.45; resize: vertical; }
  .add textarea:focus-visible, .edit-box:focus-visible { outline: 2px solid var(--accent-strong); outline-offset: 1px; }
  .items { margin: 12px 0 0; padding: 0; list-style: none; }
  .item { padding: 8px 0; border-top: 1px solid var(--line); }
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
  .finished { display: flex; align-items: center; gap: 8px; margin-top: 10px; color: var(--muted); font-size: var(--text-meta); }
  .note { margin: 10px 0 0; color: var(--muted); font-size: var(--text-body); }
  .error { margin: 8px 0 0; color: var(--danger); font-size: var(--text-body); }
</style>
