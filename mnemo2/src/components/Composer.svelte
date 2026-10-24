<script lang="ts">
  import { ArrowUp, Sparkles } from "@lucide/svelte";
  import { actions } from "../lib/store";

  let draft = $state("");

  function send() {
    const message = draft.trim();
    if (!message) return;
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
    <button class="send-button" type="submit" aria-label="Send message" disabled={!draft.trim()}>
      <ArrowUp size={17} strokeWidth={2} />
    </button>
  </div>
  <div class="composer-meta">
    <span><Sparkles size={12} /> Lead · Atlas Core</span>
    <span>Enter to send · Shift + Enter for a new line</span>
  </div>
</form>

<style>
  .composer { width: min(100%, 820px); margin: 0 auto; }
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
  .composer-meta { display: flex; justify-content: space-between; gap: 12px; padding: 8px 4px 0; color: var(--muted); font-size: 9px; }
  .composer-meta span:first-child { display: inline-flex; align-items: center; gap: 5px; color: var(--ink-soft); }
  @media (max-width: 560px) { .composer-meta span:last-child { display: none; } }
</style>
