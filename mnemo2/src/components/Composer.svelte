<script lang="ts">
  import { ArrowUp, Square, X } from "@lucide/svelte";
  import { parseNumericAnswer } from "../../../frontend/src/lib/answerSelect";
  import { fileToAttachedDataUrl } from "../../../frontend/src/lib/attachImages";
  import { actions, app } from "../lib/store";
  import LeadModelPicker from "./LeadModelPicker.svelte";

  let stopping = $state(false);
  let attaching = $state(false);
  // The draft lives in store state (not component state) so unmounting the
  // Lead view — for example by opening the Team view — never loses it.
  const draft = $derived($app.leadDraft);
  const busy = $derived($app.leadRuntime.busy);
  const pending = $derived($app.leadRuntime.pending !== null);
  const connecting = $derived(
    $app.leadRuntime.mode === "native" && $app.leadRuntime.status === "opening",
  );
  const canInterrupt = $derived(
    $app.leadRuntime.mode === "native" &&
      $app.leadRuntime.status === "ready" &&
      (busy || pending),
  );
  // A busy native lead accepts a steer, so sending stays available unless a
  // pending question/approval owns the turn (answer that first).
  const canSend = $derived(
    !pending &&
      ($app.leadRuntime.mode === "mock" || $app.leadRuntime.status === "ready"),
  );
  const hasContent = $derived(
    draft.text.trim().length > 0 || draft.images.length > 0,
  );
  // A pending question is answered from this same box: a numeric `N)` picks an
  // option, anything else is a freeform answer. Approvals stay on their buttons.
  const pendingQuestion = $derived(
    $app.leadRuntime.pending?.kind === "question" &&
      !!$app.leadRuntime.pending.question_id
      ? $app.leadRuntime.pending
      : null,
  );
  const canAnswerQuestion = $derived(
    pendingQuestion !== null && $app.leadRuntime.status === "ready",
  );
  const canSubmit = $derived(
    canAnswerQuestion ? draft.text.trim().length > 0 : hasContent,
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
    if (canAnswerQuestion && pendingQuestion) {
      submitAnswer(pendingQuestion);
      return;
    }
    if (!canSend) return;
    const message = draft.text.trim();
    const images = draft.images;
    // An image-only submission is a real message for a vision model.
    if (!message && images.length === 0) return;
    actions.sendLeadMessage(message, images);
    actions.clearLeadDraft();
  }

  function submitAnswer(question: NonNullable<typeof pendingQuestion>) {
    const questionId = question.question_id;
    const answer = draft.text.trim();
    if (!questionId || !answer) return;
    const parsed = parseNumericAnswer(answer, question.options?.length ?? 0);
    if (parsed?.kind === "choice") {
      actions.answerLead(questionId, { kind: "choice", index: parsed.index });
    } else {
      // Any other text (including a bare number) is a freeform answer.
      actions.answerLead(questionId, { kind: "freeform", text: answer });
    }
    actions.clearLeadDraft();
  }

  function handleInput(event: Event) {
    actions.setLeadDraftText((event.currentTarget as HTMLTextAreaElement).value);
  }

  function imageFiles(data: DataTransfer | null): File[] {
    return Array.from(data?.items ?? [])
      .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
      .map((item) => item.getAsFile())
      .filter((file): file is File => file !== null);
  }

  async function handlePaste(event: ClipboardEvent) {
    const files = imageFiles(event.clipboardData);
    if (files.length === 0) return;
    // Only swallow the paste when it actually carries an image; ordinary
    // text pastes (and the browser's undo stack) keep working untouched.
    event.preventDefault();
    attaching = true;
    try {
      // Downscale/re-encode through the shared attachment pipeline before the
      // data URL ever reaches the store, exactly like the Mnemo1 composer.
      const attached = await Promise.allSettled(
        files.map((file) => fileToAttachedDataUrl(file)),
      );
      for (const result of attached) {
        if (result.status === "fulfilled") actions.addLeadDraftImage(result.value);
      }
    } finally {
      attaching = false;
    }
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
  {#if draft.images.length > 0}
    <ul class="draft-images" aria-label="Attached images">
      {#each draft.images as image, index}
        <li class="draft-image">
          <img src={image} alt={`Attachment ${index + 1}`} />
          <button
            type="button"
            class="remove-image"
            aria-label={`Remove attachment ${index + 1}`}
            title="Remove attachment"
            onclick={() => actions.removeLeadDraftImage(index)}
          >
            <X size={11} strokeWidth={2.5} />
          </button>
        </li>
      {/each}
    </ul>
  {/if}
  <div class="composer-input">
    <textarea
      value={draft.text}
      oninput={handleInput}
      onpaste={handlePaste}
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
    {/if}
    {#if canAnswerQuestion || canSend}
      <button
        class="send-button"
        type="submit"
        aria-label={canAnswerQuestion
          ? "Answer question"
          : busy
            ? "Steer the lead"
            : "Send message"}
        title={canAnswerQuestion
          ? "Answer question"
          : busy
            ? "Steer the lead"
            : "Send message"}
        disabled={!canSubmit}
      >
        <ArrowUp size={17} strokeWidth={2} />
      </button>
    {:else if !canInterrupt}
      <button
        class="send-button"
        type="submit"
        aria-label="Send message"
        title="Send message"
        disabled
      >
        <ArrowUp size={17} strokeWidth={2} />
      </button>
    {/if}
  </div>
  <div class="composer-meta">
    <LeadModelPicker compact />
    <div class="composer-status" aria-live="polite">
      {#if connecting}<span>Connecting</span>{/if}
      {#if attaching}<span>Attaching image</span>{/if}
    </div>
  </div>
</form>

<style>
  .composer { width: 100%; margin: 0; }
  .draft-images {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin: 0 0 8px;
    padding: 0;
    list-style: none;
  }
  .draft-image {
    position: relative;
    width: 52px;
    height: 52px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--surface-soft);
  }
  .draft-image img {
    display: block;
    width: 100%;
    height: 100%;
    border-radius: 9px;
    object-fit: cover;
  }
  .remove-image {
    position: absolute;
    top: -5px;
    right: -5px;
    display: grid;
    width: 16px;
    height: 16px;
    padding: 0;
    place-items: center;
    border: 1px solid var(--line-strong);
    border-radius: 50%;
    background: var(--surface);
    color: var(--ink);
    cursor: pointer;
  }
  .remove-image:hover { background: var(--line); }
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
