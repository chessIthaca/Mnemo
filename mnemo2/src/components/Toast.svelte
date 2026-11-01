<script lang="ts">
  import { X } from "@lucide/svelte";
  import { actions, app } from "../lib/store";

  let timer: ReturnType<typeof setTimeout> | undefined;

  $effect(() => {
    const toast = $app.toast;
    if (!toast) return;
    clearTimeout(timer);
    timer = setTimeout(() => actions.dismissToast(), 4200);
    return () => clearTimeout(timer);
  });
</script>

{#if $app.toast}
  <div class="toast" role="status" aria-live="polite">
    <span>{$app.toast.message}</span>
    <button aria-label="Dismiss notification" onclick={actions.dismissToast}><X size={13} /></button>
  </div>
{/if}

<style>
  .toast { position: fixed; bottom: 20px; left: 20px; z-index: 80; display: flex; align-items: center; gap: 12px; max-width: min(420px, calc(100vw - 40px)); padding: 11px 12px 11px 14px; border: 1px solid var(--line-strong); border-radius: 11px; background: var(--ink); color: #f2f4ec; box-shadow: var(--shadow); font-size: var(--text-sm); line-height: 1.45; animation: toast-in 180ms ease-out; }
  .toast button { display: grid; flex: 0 0 auto; width: 22px; height: 22px; place-items: center; border: 0; border-radius: 6px; background: rgb(255 255 255 / 9%); color: #cfd5c7; cursor: pointer; }
  @keyframes toast-in { from { opacity: 0; transform: translateY(5px); } }
  @media (max-width: 760px) { .toast { right: 16px; bottom: 74px; left: 16px; max-width: none; } }
</style>
