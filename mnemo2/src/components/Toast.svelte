<script lang="ts">
  import { Check, Copy, X } from "@lucide/svelte";
  import { actions, app } from "../lib/store";

  /** How long an info popup lingers — it confirms something already done. */
  const INFO_MS = 4200;
  /** A failure stays long enough to be read, copied, or dismissed by hand. */
  const ERROR_MS = 12_000;

  let timer: ReturnType<typeof setTimeout> | undefined;
  /** The countdown's remaining milliseconds, kept while a pointer rests here. */
  let remaining = 0;
  let expiresAt = 0;
  let copied = $state(false);

  function clear() {
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
  }

  /** Start (or restart) the countdown from `ms`. */
  function arm(ms: number) {
    clear();
    remaining = ms;
    expiresAt = Date.now() + ms;
    timer = setTimeout(() => actions.dismissToast(), ms);
  }

  /** Hovering pauses the countdown, so a long message can be read in full. */
  function pause() {
    if (timer === undefined) return;
    clear();
    remaining = Math.max(0, expiresAt - Date.now());
  }

  function resume() {
    if (timer !== undefined || remaining <= 0) return;
    arm(remaining);
  }

  $effect(() => {
    const toast = $app.toast;
    copied = false;
    if (!toast) {
      clear();
      return;
    }
    arm(toast.level === "error" ? ERROR_MS : INFO_MS);
    return clear;
  });

  async function copyMessage() {
    const toast = $app.toast;
    if (!toast) return;
    try {
      await navigator.clipboard.writeText(toast.message);
      copied = true;
    } catch {
      // A refused clipboard leaves the text on screen; nothing else to do.
    }
  }
</script>

{#if $app.toast}
  <div
    class="toast"
    class:error={$app.toast.level === "error"}
    role={$app.toast.level === "error" ? "alert" : "status"}
    aria-live={$app.toast.level === "error" ? "assertive" : "polite"}
    onmouseenter={pause}
    onmouseleave={resume}
    onfocusin={pause}
    onfocusout={resume}
  >
    <span class="message">{$app.toast.message}</span>
    {#if $app.toast.level === "error"}
      <button type="button" class="copy" aria-label="Copy notification" title="Copy notification" onclick={copyMessage}>
        {#if copied}<Check size={13} />{:else}<Copy size={13} />{/if}
      </button>
    {/if}
    <button type="button" aria-label="Dismiss notification" title="Dismiss notification" onclick={actions.dismissToast}><X size={13} /></button>
  </div>
{/if}

<style>
  .toast { position: fixed; bottom: 20px; left: 20px; z-index: 80; display: flex; align-items: center; gap: 12px; max-width: min(420px, calc(100vw - 40px)); padding: 11px 12px 11px 14px; border: 1px solid var(--line-strong); border-radius: 11px; background: var(--toast-bg); color: var(--toast-ink); box-shadow: var(--shadow); font-size: var(--text-body); line-height: 1.45; animation: toast-in 180ms ease-out; }
  .toast.error { border-color: var(--danger-line); }
  .toast .message { flex: 1 1 auto; }
  .toast button { display: grid; flex: 0 0 auto; width: 22px; height: 22px; place-items: center; border: 0; border-radius: 6px; background: var(--toast-button-bg); color: var(--toast-button-ink); cursor: pointer; }
  @keyframes toast-in { from { opacity: 0; transform: translateY(5px); } }
  @media (max-width: 760px) { .toast { right: 16px; bottom: 74px; left: 16px; max-width: none; } }
</style>
