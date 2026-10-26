<script lang="ts">
  import { ArrowUp, Square } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import LeadModelPicker from "./LeadModelPicker.svelte";

  let draft = $state("");
  let stopping = $state(false);
  const busy = $derived($app.leadRuntime.busy);
  const connecting = $derived(
    $app.leadRuntime.mode === "native" && $app.leadRuntime.status === "opening",
  );
  const canInterrupt = $derived(
    $app.leadRuntime.mode === "native" &&
      $app.leadRuntime.status === "ready" &&
      ($app.leadRuntime.busy || $app.leadRuntime.pending !== null),
  );
  const canSend = $derived(
    $app.leadRuntime.pending === null &&
      ($app.leadRuntime.mode === "mock" ||
        ($app.leadRuntime.status === "ready" && !busy)),
  );

  $effect(() => {
    if (!canInterrupt) stopping = false;
  });

  async function stop() {
    if (stopping) return;
    stopping = true;
    try {
      await actions.interruptLead();
    } catch {
      // The store surfaces the actual interruption failure.
      stopping = false;
    }
  }

  function send() {
    const message = draft.trim();
    if (!message || !canSend) return;
    actions.sendLeadMessage(message);
    draft = "";
  }

  function handleSubmit(event: SubmitEvent) {
    event.preventDefault();
    send();
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      send();
    }
  }
</script>

<form class="composer" onsubmit={handleSubmit}>
  <div class="composer-input">
    <textarea
      bind:value={draft}
      onkeydown={handleKeydown}
      rows="2"
      aria-label="Message the lead"
      placeholder="Ask, plan, or redirect the team…"
    ></textarea>
    {#if canInterrupt || stopping}
      <button
        class="send-button stop-button"
        type="button"
        aria-label={stopping ? "Stopping the lead" : "Stop the lead"}
        title={stopping ? "Stopping the lead" : "Stop the lead"}
        disabled={stopping}
        onclick={stop}
      >
        <Square size={12} fill="currentColor" strokeWidth={1.5} />
        <span>{stopping ? "Stopping…" : "Stop"}</span>
      </button>
    {:else}
      <button
        class="send-button"
        type="submit"
        aria-label="Send message"
        disabled={!draft.trim() || !canSend}
      >
        <ArrowUp size={17} strokeWidth={2} />
      </button>
    {/if}
  </div>
  <div class="composer-meta">
    <LeadModelPicker compact />
    <div class="composer-status" aria-live="polite">
      {#if connecting}<span>Connecting</span>{/if}
    </div>
  </div>
</form>

<style>
  .composer { width: 100%; margin: 0; }
  .composer-input {
    display: flex;
    align-items: flex-end;
    gap: 12px;
    padding: 12px 12px 10px 16px;
    border: 1px solid var(--line-strong);
    border-radius: 16px;
    background: var(--surface);
    box-shadow: 0 12px 36px rgb(37 40 32 / 7%);
    transition: border-color 160ms ease, box-shadow 160ms ease;
  }
  .composer-input:focus-within { border-color: #aeb8a5; box-shadow: 0 12px 40px rgb(37 40 32 / 9%), 0 0 0 3px var(--accent-soft); }
  textarea {
    flex: 1;
    min-height: 44px;
    max-height: 150px;
    resize: none;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--ink);
    font-size: 13px;
    line-height: 1.55;
  }
  textarea::placeholder { color: #999c91; }
  .send-button {
    display: grid;
    flex: 0 0 auto;
    width: 34px;
    height: 34px;
    place-items: center;
    border: 0;
    border-radius: 10px;
    background: var(--ink);
    color: white;
    cursor: pointer;
  }
  .send-button:disabled { background: var(--surface-soft); color: #adb0a6; cursor: default; }
  .stop-button { display: inline-flex; width: auto; min-width: 64px; gap: 6px; padding: 0 10px; border: 1px solid var(--line); background: var(--surface-soft); color: var(--ink); font-size: 11px; font-weight: 600; }
  .stop-button:hover { border-color: var(--line-strong); background: var(--line); }
  .stop-button:disabled { cursor: wait; }
  .composer-meta { display: flex; align-items: center; justify-content: space-between; gap: 10px; min-height: 14px; padding: 8px 4px 0; }
  .composer-status { display: flex; align-items: center; gap: 6px; color: var(--muted); font-size: 8px; font-weight: 600; letter-spacing: 0.04em; }
  @media (max-width: 560px) { .composer { width: 100%; } }
</style>
