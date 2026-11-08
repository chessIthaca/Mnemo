<script lang="ts">
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import {
    createLayaLearningsTransport,
    offerMessage,
    type LayaLearningsTransport,
    type LearningsOffer,
  } from "../lib/layaLearnings";
  import Sheet from "./Sheet.svelte";

  /**
   * The I/O seam. Defaults to the native transport, which is null in a plain
   * browser, so the sheet renders nothing outside the desktop app; tests pass a
   * stand-in.
   */
  let { transport: provided = null }: { transport?: LayaLearningsTransport | null } = $props();

  const transport = $derived(provided ?? createLayaLearningsTransport());

  /** The copies waiting for an answer, in the host's order. */
  let offers = $state<LearningsOffer[]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);

  /** The offer the sheet shows right now. */
  const current = $derived(offers[0] ?? null);

  onMount(() => {
    const active = transport;
    if (!active) return;
    let stopped = false;
    let unlisten: UnlistenFn | null = null;

    // The host emits the full list whenever it changed, including an empty one
    // (which closes the question).
    void active
      .onOffers((next) => {
        if (!stopped) offers = next;
      })
      .then((stop) => {
        if (stopped) stop();
        else unlisten = stop;
      })
      .catch((cause: unknown) => {
        if (!stopped) error = errorText(cause);
      });
    void active
      .offers()
      .then((next) => {
        if (!stopped) offers = next;
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
    return "the learnings update failed";
  }

  /** Answer the pending offer; `decide` returns the remaining list. */
  async function answer(accept: boolean): Promise<void> {
    const offer = current;
    const active = transport;
    if (!offer || !active || busy) return;
    busy = true;
    error = null;
    try {
      offers = await active.decide(offer, accept);
    } catch (cause) {
      error = errorText(cause);
      offers = offers.filter((candidate) => candidate.source_key !== offer.source_key);
    } finally {
      busy = false;
    }
  }
</script>

{#if current !== null}
  <Sheet
    open={true}
    title="Update Laya's learnings?"
    eyebrow="LAYA'S LEARNINGS"
    onclose={() => void answer(false)}
  >
    <p class="dialog-copy">{offerMessage(current)}</p>
    {#if error !== null}
      <p class="learnings-error" role="alert">{error}</p>
    {/if}
    <div class="dialog-actions">
      <button class="button button-quiet" disabled={busy} onclick={() => void answer(false)}>
        Not now
      </button>
      <button class="button button-primary" disabled={busy} onclick={() => void answer(true)}>
        {#if busy}
          Updating…
        {:else}
          Update
        {/if}
      </button>
    </div>
  </Sheet>
{/if}

<style>
  .dialog-copy { margin: 0 0 12px; color: var(--muted); font-size: var(--text-body); line-height: 1.65; }
  .learnings-error { margin: 0 0 12px; color: var(--danger); font-size: var(--text-body); line-height: 1.5; }
  .dialog-actions { display: flex; justify-content: flex-end; gap: 8px; padding-top: 14px; border-top: 1px solid var(--line); }
</style>
