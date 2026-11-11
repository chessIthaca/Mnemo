<script lang="ts">
  import { X } from "@lucide/svelte";
  import { get } from "svelte/store";
  import { parseNumericAnswer } from "../lib/answerSelect";
  import { fileToAttachedDataUrl } from "../lib/attachImages";
  import {
    SLASH_COMMANDS,
    filterSlashCommands,
    parseSlash,
    resolveModel,
    type SlashCommand,
    type SlashCommandInfo,
  } from "../lib/slash";
  import { actions, app } from "../lib/store";
  import type { LeadAnswerInput } from "../lib/types";
  import ComposerBox from "./ComposerBox.svelte";
  import IndexingStatus from "./IndexingStatus.svelte";
  import LeadModelPicker from "./LeadModelPicker.svelte";
  import Sheet from "./Sheet.svelte";

  let stopping = $state(false);
  let attaching = $state(false);
  let submitting = $state(false);
  let answering = $state(false);
  let freeformQuestionId = $state<string | null>(null);
  // Claude Code leads this project: sending to the built-in lead asks first.
  let confirmingExternal = $state(false);
  // The slash menu: which suggestion is highlighted, and the draft Escape
  // dismissed the menu for (it stays hidden until the draft changes).
  let menuIndex = $state(0);
  let menuDismissedFor = $state<string | null>(null);
  let helpOpen = $state(false);
  const externalLeading = $derived($app.externalLead.attached);
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
  // Selecting the trailing "let's talk" number (`options+1)`) opens freeform
  // mode instead of submitting the sentinel itself; the next submission is
  // the freeform answer text.
  const freeformMode = $derived(
    pendingQuestion !== null &&
      freeformQuestionId === pendingQuestion.question_id,
  );
  // The command suggestions: only while the draft is still a command name,
  // and never in freeform-answer mode (where a slash is ordinary text).
  const menu = $derived(
    freeformMode || menuDismissedFor === draft.text
      ? []
      : filterSlashCommands(draft.text),
  );
  let lastMenuDraft = "";
  $effect(() => {
    // A changed draft reshapes the menu; the highlight returns to its top.
    if (draft.text === lastMenuDraft) return;
    lastMenuDraft = draft.text;
    menuIndex = 0;
  });

  /** The open `ask_user` question this box answers, if any. */
  const questionId = $derived(
    $app.leadRuntime.pending?.kind === "question" ? $app.leadRuntime.pending.question_id ?? null : null,
  );

  /** The one button beside the box: answer, steer, send, or a disabled send. */
  const sendButton = $derived(
    canAnswerQuestion || canSend
      ? {
          ariaLabel: canAnswerQuestion ? "Answer question" : busy ? "Steer the lead" : "Send message",
          enabled: canSubmit && !submitting && !answering,
        }
      : canInterrupt
        ? null
        : { ariaLabel: "Send message", enabled: false },
  );
  /** Stop while the lead runs; a stopped lead offers send again once it settles. */
  const stopButton = $derived(
    canInterrupt || stopping
      ? {
          ariaLabel: stopping ? "Stopping the lead" : "Stop the lead",
          text: stopping ? "Stopping…" : "Stop",
          busy: stopping,
          onclick: () => void stop(),
        }
      : null,
  );

  $effect(() => {
    if (!canInterrupt) stopping = false;
  });
  $effect(() => {
    if (!externalLeading) confirmingExternal = false;
  });
  $effect(() => {
    if (
      pendingQuestion === null ||
      pendingQuestion.question_id !== freeformQuestionId
    ) {
      freeformQuestionId = null;
    }
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

  /** Replace the draft with a command's usage, ready for its argument. */
  function completeCommand(command: SlashCommandInfo) {
    actions.setLeadDraftText(
      command.takesArgument ? `/${command.name} ` : `/${command.name}`,
    );
  }

  /** Whether the draft is exactly this argument-less command, so it can run. */
  function typedOut(command: SlashCommandInfo): boolean {
    return (
      !command.takesArgument &&
      draft.text.trim().toLowerCase() === `/${command.name}`
    );
  }

  /** Pick a menu entry with the mouse: an argument-less command runs at once. */
  function activateCommand(command: SlashCommandInfo) {
    if (command.takesArgument) {
      completeCommand(command);
      return;
    }
    actions.setLeadDraftText(`/${command.name}`);
    void send();
  }

  /** The menu's keys; true when the key belonged to the menu. */
  function handleMenuKeydown(event: KeyboardEvent): boolean {
    if (menu.length === 0) return false;
    const highlighted = menu[Math.min(menuIndex, menu.length - 1)];
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      menuIndex = (menuIndex + step + menu.length) % menu.length;
      return true;
    }
    if (event.key === "Tab") {
      event.preventDefault();
      completeCommand(highlighted);
      return true;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      // Enter runs a command only once its name is typed out: completing a
      // partial name first keeps "/c" + Enter from running /clear by accident.
      if (typedOut(highlighted)) activateCommand(highlighted);
      else completeCommand(highlighted);
      return true;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      menuDismissedFor = draft.text;
      return true;
    }
    return false;
  }

  /** Run a parsed command; a refusal toasts and leaves the draft untouched. */
  async function runSlash(command: SlashCommand) {
    switch (command.kind) {
      case "invalid":
        actions.showToast(command.message, "error");
        return;
      case "help":
        helpOpen = true;
        actions.setLeadDraftText("");
        return;
      case "new":
        if (await actions.newLeadConversation()) actions.setLeadDraftText("");
        return;
      case "compact":
        await actions.compactLead();
        actions.setLeadDraftText("");
        return;
      case "stop":
        if (canInterrupt) await stop();
        else actions.showToast("The lead is not working right now.");
        actions.setLeadDraftText("");
        return;
      case "model":
        runModelCommand(command.name, command.effort);
        return;
    }
  }

  /** Apply /model; an unknown model or effort keeps the draft for a retry. */
  function runModelCommand(name: string, effort: string | null) {
    const models = $app.leadRuntime.models;
    if (!name) {
      actions.showToast(
        `Usage: /model <name> [effort]. Models: ${models.map((model) => model.spec).join(", ")}`,
        "error",
      );
      return;
    }
    const resolved = resolveModel(name, models);
    if ("error" in resolved) {
      actions.showToast(resolved.error, "error");
      return;
    }
    if (effort !== null) {
      const efforts =
        models.find((model) => model.spec === resolved.spec)?.reasoning_efforts ?? [];
      if (!efforts.includes(effort)) {
        actions.showToast(
          efforts.length === 0
            ? `${resolved.spec} takes no reasoning effort.`
            : `${resolved.spec} takes: ${efforts.join(", ")}.`,
          "error",
        );
        return;
      }
    }
    actions.setLeadModel(resolved.spec, effort);
    actions.setLeadDraftText("");
  }

  async function send(confirmedExternal = false) {
    // A slash command runs here, ahead of the answer branch and the send gate:
    // commands work while the lead is busy or holding a pending interaction.
    if (!freeformMode) {
      const command = parseSlash(draft.text);
      if (command) {
        await runSlash(command);
        return;
      }
    }
    // A confirmed send is the prompt the user approved, even if a question
    // arrived while the confirmation was open.
    if (!confirmedExternal && canAnswerQuestion && pendingQuestion) {
      await submitAnswer(pendingQuestion);
      return;
    }
    if (!canSend || submitting) return;
    const message = draft.text.trim();
    const images = draft.images;
    // An image-only submission is a real message for a vision model.
    if (!message && images.length === 0) return;
    // Cancelling the confirmation keeps the draft untouched.
    if (externalLeading && !confirmedExternal) {
      confirmingExternal = true;
      return;
    }
    const submittedText = draft.text;
    const submittedImages = [...images];
    submitting = true;
    try {
      // Clear only after the host confirms acceptance: a rejected or raced
      // submission keeps the draft instead of destroying text/attachments.
      const accepted = await actions.sendLeadMessage(message, images);
      if (!accepted) return;
      const current = get(app).leadDraft;
      const unchanged =
        current.text === submittedText &&
        current.images.length === submittedImages.length &&
        current.images.every((image, index) => image === submittedImages[index]);
      if (unchanged) actions.clearLeadDraft();
    } finally {
      submitting = false;
    }
  }

  function sendAnyway() {
    confirmingExternal = false;
    void send(true);
  }

  async function submitAnswer(question: NonNullable<typeof pendingQuestion>) {
    if (answering) return;
    const questionId = question.question_id;
    const answer = draft.text.trim();
    if (!questionId || !answer) return;

    let payload: LeadAnswerInput;
    if (freeformMode) {
      payload = { kind: "freeform", text: answer };
    } else {
      const parsed = parseNumericAnswer(answer, question.options?.length ?? 0);
      if (parsed?.kind === "choice") {
        payload = { kind: "choice", index: parsed.index };
      } else if (parsed?.kind === "freeform") {
        // The "let's talk" number is a mode switch, not an answer: never
        // submit the sentinel text. Clear just the command, keep attachments.
        freeformQuestionId = questionId;
        actions.setLeadDraftText("");
        return;
      } else {
        // Any other text (including a bare number) is a freeform answer.
        payload = { kind: "freeform", text: answer };
      }
    }

    answering = true;
    try {
      // Clear only after the host accepts the answer. Answers are text-only,
      // so attached images stay in the draft even on success.
      if (await actions.answerLead(questionId, payload)) {
        freeformQuestionId = null;
        actions.setLeadDraftText("");
      }
    } finally {
      answering = false;
    }
  }

  function imageFiles(data: DataTransfer | null): File[] {
    const fromItems = Array.from(data?.items ?? [])
      .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
      .map((item) => item.getAsFile())
      .filter((file): file is File => file !== null);
    // An image FILE copied in File Explorer pastes with a file-kind item
    // carrying an EMPTY type (never image/*) — the real image File only
    // surfaces in clipboardData.files. Items win first, so an ordinary
    // screenshot paste (which Chromium lists in BOTH) can never double-attach.
    if (fromItems.length > 0) return fromItems;
    return Array.from(data?.files ?? []).filter((file) =>
      file.type.startsWith("image/"),
    );
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
      // data URL ever reaches the store.
      const attached = await Promise.allSettled(
        files.map((file) => fileToAttachedDataUrl(file)),
      );
      for (const result of attached) {
        if (result.status === "fulfilled") actions.addLeadDraftImage(result.value);
      }
      // Never swallow a rejected conversion: the user must know the image
      // never made it into the draft.
      const failed = attached.filter((result) => result.status === "rejected").length;
      if (failed > 0) {
        actions.showToast(
          failed === 1
            ? "Could not attach the pasted image."
            : `Could not attach ${failed} pasted images.`,
          "error",
        );
      }

    } finally {
      attaching = false;
    }
  }

  function handleSubmit(event: SubmitEvent) {
    event.preventDefault();
    send();
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
  {#if menu.length > 0}
    <ul class="slash-menu" role="listbox" aria-label="Slash commands">
      {#each menu as command, index (command.name)}
        <li
          class="slash-item"
          class:selected={index === menuIndex}
          role="option"
          aria-selected={index === menuIndex}
          onmousedown={(event) => {
            // Keep the textarea focused: a blur would swallow the click.
            event.preventDefault();
            activateCommand(command);
          }}
        >
          <span class="slash-usage">{command.usage}</span>
          <span class="slash-description">{command.description}</span>
        </li>
      {/each}
    </ul>
  {/if}
  <ComposerBox
    value={draft.text}
    label="Message the lead"
    placeholder={freeformMode
      ? "Type your freeform answer…"
      : "Ask, plan, or redirect the team — / for commands"}
    send={sendButton}
    stop={stopButton}
    oninput={actions.setLeadDraftText}
    onsubmit={() => void send()}
    onpaste={handlePaste}
    onkeydown={handleMenuKeydown}
    onescape={freeformMode ? () => { freeformQuestionId = null; } : undefined}
  />

  <div class="composer-meta">
    <LeadModelPicker compact />
    <div class="composer-status" aria-live="polite">
      {#if connecting}<span>Connecting</span>{/if}
      {#if attaching}<span>Attaching image</span>{/if}
      {#if freeformMode}<span>Freeform answer — Enter to send, Esc to cancel</span>{/if}
      {#if submitting}<span>Sending</span>{/if}
      <IndexingStatus />
    </div>
  </div>
</form>

<Sheet open={confirmingExternal} title="Send to the built-in lead?" eyebrow="EXTERNAL LEAD" onclose={() => (confirmingExternal = false)}>
  <p class="confirm-copy">Claude Code is leading this project. Send this to the built-in lead anyway?</p>
  <div class="confirm-actions">
    <button class="button button-quiet" type="button" onclick={() => (confirmingExternal = false)}>Cancel</button>
    <button class="button button-primary" type="button" onclick={sendAnyway}>Send anyway</button>
  </div>
</Sheet>

<Sheet open={helpOpen} title="Slash commands" eyebrow="LEAD" onclose={() => (helpOpen = false)}>
  <ul class="slash-help">
    {#each SLASH_COMMANDS as command (command.name)}
      <li>
        <code>{command.usage}</code>
        <span>{command.description}</span>
      </li>
    {/each}
  </ul>
  <p class="slash-help-note">Anything else that starts with / is sent to the lead as a normal message.</p>
</Sheet>

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
  .composer-meta { display: flex; align-items: center; justify-content: space-between; gap: 10px; min-height: 14px; padding: 8px 4px 0; }

  .slash-menu { display: flex; flex-direction: column; gap: 2px; margin: 0 0 8px; padding: 4px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); list-style: none; }
  .slash-item { display: flex; align-items: baseline; gap: 10px; padding: 6px 8px; border-radius: 8px; cursor: pointer; }
  .slash-item.selected { background: var(--surface-soft); }
  .slash-usage { flex: 0 0 auto; color: var(--ink); font-family: var(--font-mono); font-size: var(--text-meta); font-weight: 600; }
  .slash-description { min-width: 0; overflow: hidden; color: var(--muted); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .slash-help { display: flex; flex-direction: column; gap: 10px; margin: 0 0 16px; padding: 0; list-style: none; }
  .slash-help li { display: flex; flex-direction: column; gap: 2px; }
  .slash-help code { color: var(--ink); font-family: var(--font-mono); font-size: var(--text-body); font-weight: 600; }
  .slash-help span { color: var(--muted); font-size: var(--text-meta); }
  .slash-help-note { margin: 0; padding-top: 12px; border-top: 1px solid var(--line); color: var(--muted); font-size: var(--text-meta); }
  .composer-status { display: flex; align-items: center; gap: 12px; color: var(--muted); font-size: var(--text-meta); font-weight: 600; letter-spacing: 0.04em; }

  .confirm-copy { margin: 0 0 16px; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }
  .confirm-actions { display: flex; justify-content: space-between; gap: 8px; padding-top: 16px; border-top: 1px solid var(--line); }
  @media (max-width: 560px) { .composer { width: 100%; } }
</style>
