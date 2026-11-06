<script lang="ts">
  /**
   * One confirmation idiom for "leaving now would drop an edit": the question
   * plus a safe and a destructive answer. "Keep editing" takes focus when the
   * banner appears, so a stray Enter or Escape loses nothing.
   */
  let {
    message,
    onkeep,
    ondiscard,
  }: { message: string; onkeep: () => void; ondiscard: () => void } = $props();

  let keepButton = $state<HTMLButtonElement | null>(null);

  $effect(() => {
    keepButton?.focus();
  });
</script>

<div class="inline-confirm" role="alert">
  <span>{message}</span>
  <button class="button button-quiet" bind:this={keepButton} onclick={onkeep}>Keep editing</button>
  <button class="button button-primary" onclick={ondiscard}>Discard changes</button>
</div>

<style>
  .inline-confirm { display: flex; align-items: center; justify-content: flex-end; gap: 8px; padding: 8px 22px; border-bottom: 1px solid var(--line); background: var(--warning-soft); color: var(--warning); font-size: var(--text-body); }
  .inline-confirm span { margin-right: auto; font-weight: 600; }
</style>
