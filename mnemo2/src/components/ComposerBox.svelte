<script module lang="ts">
  /** One button beside the box: its accessible name and whether it takes a click. */
  export interface BoxAction {
    ariaLabel: string;
    enabled: boolean;
  }

  /** The interrupt button beside the box. */
  export interface BoxStop {
    ariaLabel: string;
    text: string;
    busy: boolean;
    onclick: () => void;
  }
</script>

<script lang="ts">
  import { ArrowUp, Square } from "@lucide/svelte";

  /**
   * The message box itself — the shared shell the lead's composer and every
   * worker's reply box render, so both look and behave the same.
   *
   * It owns no state and reaches no store: the parent supplies the draft text,
   * the label, the placeholder and the buttons, and supplies its own `<form>`
   * (this box renders none) so submission stays with whoever owns the action.
   */
  let {
    value,
    label,
    placeholder,
    disabled = false,
    send,
    stop = null,
    oninput,
    onsubmit,
    onpaste,
    onescape,
  }: {
    value: string;
    label: string;
    placeholder: string;
    disabled?: boolean;
    send: BoxAction | null;
    stop?: BoxStop | null;
    oninput: (text: string) => void;
    onsubmit: () => void;
    onpaste?: (event: ClipboardEvent) => void;
    onescape?: () => void;
  } = $props();

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && onescape) {
      event.preventDefault();
      onescape();
      return;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      if (!disabled) onsubmit();
    }
  }
</script>

<div class="composer-input">
  <textarea
    {value}
    oninput={(event) => oninput(event.currentTarget.value)}
    {onpaste}
    onkeydown={handleKeydown}
    rows="2"
    aria-label={label}
    {placeholder}
    {disabled}
  ></textarea>
  {#if stop}
    <button
      class="send-button stop-button"
      type="button"
      aria-label={stop.ariaLabel}
      title={stop.ariaLabel}
      disabled={stop.busy}
      onclick={stop.onclick}
    >
      <Square size={12} fill="currentColor" strokeWidth={1.5} />
      <span>{stop.text}</span>
    </button>
  {/if}
  {#if send}
    <button
      class="send-button"
      type="submit"
      aria-label={send.ariaLabel}
      title={send.ariaLabel}
      disabled={!send.enabled}
    >
      <ArrowUp size={17} strokeWidth={2} />
    </button>
  {/if}
</div>

<style>
  .composer-input {
    display: flex;
    align-items: flex-end;
    gap: 12px;
    padding: 12px 12px 10px 16px;
    border: 1px solid var(--line-strong);
    border-radius: 16px;
    background: var(--surface);
    box-shadow: var(--shadow-composer);
    transition: border-color 160ms ease, box-shadow 160ms ease;
  }
  .composer-input:focus-within { border-color: var(--focus-line); box-shadow: var(--shadow-composer-focus), 0 0 0 3px var(--accent-soft); }
  .composer-input:has(textarea:disabled) { background: var(--surface-soft); box-shadow: none; }
  textarea {
    flex: 1;
    min-height: 44px;
    max-height: 150px;
    resize: none;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--ink);
    font-size: var(--text-body);
    line-height: 1.55;
  }
  textarea::placeholder { color: var(--placeholder); }
  textarea:disabled { color: var(--muted); cursor: not-allowed; }
  .send-button {
    display: grid;
    flex: 0 0 auto;
    width: 34px;
    height: 34px;
    place-items: center;
    border: 0;
    border-radius: 10px;
    background: var(--primary-bg);
    color: var(--primary-ink);
    cursor: pointer;
  }
  .send-button:disabled { background: var(--surface-soft); color: var(--disabled-ink); cursor: default; }
  .stop-button { display: inline-flex; width: auto; min-width: 64px; gap: 6px; padding: 0 10px; border: 1px solid var(--line); background: var(--surface-soft); color: var(--ink); font-size: var(--text-body); font-weight: 600; }
  .stop-button:hover { border-color: var(--line-strong); background: var(--line); }
  .stop-button:disabled { cursor: wait; }
</style>
