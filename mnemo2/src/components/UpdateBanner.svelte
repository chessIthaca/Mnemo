<script lang="ts">
  import { actions } from "../lib/store";
  import { updates, type UpdateState, type UpdatesClient } from "../lib/updates";

  /**
   * The update banner: the small corner card that says a downloaded
   * release is ready and offers the one restart that installs it. It
   * appears only in the "ready" phase — the host checks and downloads
   * quietly on its own — and it never installs anything on a plain quit.
   */
  let { client = updates }: { client?: UpdatesClient } = $props();

  /** The latest state the host reported; null until the first answer. */
  let updateState = $state<UpdateState | null>(null);
  /** The version the user postponed with Later; a different version shows again. */
  let dismissedVersion = $state<string | null>(null);
  /** The running-worker count awaiting Restart anyway; null shows the plain card. */
  let confirmCount = $state<number | null>(null);
  /** True while a restart call is pending; locks every button. */
  let restarting = $state(false);

  /** The ready state, or null while the card stays hidden. */
  const ready = $derived(
    updateState !== null && updateState.phase === "ready" ? updateState : null,
  );
  /** The card shows for a ready version the user has not postponed. */
  const visible = $derived(ready !== null && ready.version !== dismissedVersion);

  $effect(() => {
    const active = client;
    let cancelled = false;
    let unsubscribe: (() => void) | null = null;
    void active.state().then(accept, () => {
      // A broken host command leaves the card hidden; the event may still arrive.
    });
    void active.onState(accept).then(
      (stop) => {
        unsubscribe = stop;
        if (cancelled) stop();
      },
      () => {
        // Without the event listener the card just misses live state changes.
      },
    );
    return () => {
      cancelled = true;
      unsubscribe?.();
    };
  });

  /** Take a freshly reported state; leaving "ready" drops any worker warning. */
  function accept(next: UpdateState): void {
    updateState = next;
    if (next.phase !== "ready") confirmCount = null;
  }

  /**
   * Ask the host to restart into the downloaded update. Without `force` the
   * host may answer with the running-worker count instead, switching the
   * card to its confirm mode; `force` confirms interrupting them.
   */
  async function restart(force: boolean): Promise<void> {
    if (ready === null || restarting) return;
    restarting = true;
    try {
      const answer = await client.restart(force);
      confirmCount = answer.result === "workers_running" ? answer.count : null;
    } catch (error) {
      confirmCount = null;
      actions.showToast(error instanceof Error ? error.message : String(error), "error");
    } finally {
      restarting = false;
    }
  }

  /** Postpone this version; the card returns when another version is ready. */
  function later(): void {
    if (ready !== null) dismissedVersion = ready.version;
  }

  /** Drop the worker warning and go back to the plain card. */
  function keepWorking(): void {
    confirmCount = null;
  }
</script>

{#if visible}
  <div class="update-banner" role="status" aria-live="polite">
    {#if confirmCount !== null}
      <span class="message">
        {confirmCount} worker(s) are still running. Restarting interrupts them; their branches and transcripts are kept.
      </span>
      <div class="banner-actions">
        <button type="button" class="primary" disabled={restarting} onclick={() => void restart(true)}>Restart anyway</button>
        <button type="button" disabled={restarting} onclick={keepWorking}>Keep working</button>
      </div>
    {:else}
      <span class="message">Mnemo2 {ready?.version} is ready.</span>
      <div class="banner-actions">
        <button type="button" class="primary" disabled={restarting} onclick={() => void restart(false)}>Restart now</button>
        <button type="button" disabled={restarting} onclick={later}>Later</button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .update-banner { position: fixed; bottom: 20px; left: 20px; z-index: 80; display: flex; align-items: center; gap: 12px; max-width: min(460px, calc(100vw - 40px)); padding: 11px 12px 11px 14px; border: 1px solid var(--line-strong); border-radius: 11px; background: var(--toast-bg); color: var(--toast-ink); box-shadow: var(--shadow); font-size: var(--text-body); line-height: 1.45; animation: banner-in 180ms ease-out; }
  .update-banner .message { flex: 1 1 auto; overflow-wrap: anywhere; }
  .update-banner .banner-actions { display: flex; flex: 0 0 auto; gap: 6px; }
  .update-banner button { min-height: 26px; padding: 4px 10px; border: 0; border-radius: 6px; background: var(--toast-button-bg); color: var(--toast-button-ink); cursor: pointer; font-weight: 620; }
  .update-banner button:hover:not(:disabled) { filter: brightness(1.18); }
  .update-banner button.primary { background: var(--toast-ink); color: var(--toast-bg); }
  .update-banner button:disabled { opacity: 0.45; cursor: not-allowed; }
  @keyframes banner-in { from { opacity: 0; transform: translateY(5px); } }
  @media (max-width: 760px) { .update-banner { right: 16px; bottom: 74px; left: 16px; max-width: none; } }
</style>
