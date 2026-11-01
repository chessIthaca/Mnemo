<script lang="ts">
  import { MessageCircleQuestionMark, ShieldCheck } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import Markdown from "./Markdown.svelte";

  // Questions and approvals dock on the composer, so the conversation above
  // stays readable while the human decides.
  const pending = $derived(
    $app.leadRuntime.pending?.kind === "question" || $app.leadRuntime.pending?.kind === "approval"
      ? $app.leadRuntime.pending
      : null,
  );
</script>

{#if pending}
  <section
    class="pending-prompt"
    aria-label={pending.kind === "question" ? "Question from the lead" : "Approval required"}
  >
    {#if pending.kind === "approval"}
      <header>
        <span class="prompt-icon"><ShieldCheck size={14} /></span>
        <span class="prompt-label">APPROVAL REQUIRED</span>
      </header>
      <p class="prompt-text">
        {pending.tool_name ? `${pending.tool_name} is waiting for approval.` : "A gated action is waiting for approval."}
      </p>
      <div class="prompt-actions">
        <button class="button button-primary" disabled={!pending.request_id} onclick={() => pending.request_id && actions.approveLead(pending.request_id, "approve")}>Approve</button>
        <button class="button button-quiet" disabled={!pending.request_id} onclick={() => pending.request_id && actions.approveLead(pending.request_id, "deny")}>Deny</button>
      </div>
    {:else}
      <header>
        <span class="prompt-icon"><MessageCircleQuestionMark size={14} /></span>
        <span class="prompt-label">THE LEAD ASKS</span>
      </header>
      <div class="prompt-scroll">
        {#if pending.question}
          <div class="prompt-question"><Markdown source={pending.question} onOpenLink={actions.openBrowser} /></div>
        {:else}
          <p class="prompt-text">The lead asked a question, but its text did not reach this view.</p>
        {/if}
        {#if (pending.options ?? []).length > 0}
          <div class="prompt-options">
            {#each pending.options ?? [] as option, index}
              <button
                class="prompt-option"
                disabled={!pending.question_id}
                onclick={() => pending.question_id && actions.answerLead(pending.question_id, { kind: "choice", index })}
              >
                <span class="option-index">{index + 1}</span>
                <span class="option-copy">
                  <strong>{option.label}</strong>
                  {#if option.description}<span>{option.description}</span>{/if}
                </span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
      <p class="prompt-hint">
        {(pending.options ?? []).length > 0 ? "Pick an option, or type a different answer below." : "Type your answer below and send it."}
      </p>
    {/if}
  </section>
{/if}

<style>
  .pending-prompt {
    display: flex;
    max-height: min(46vh, 420px);
    flex-direction: column;
    margin: 0 10px -1px;
    padding: 12px 14px 10px;
    border: 1px solid var(--line-strong);
    border-bottom: 0;
    border-radius: 14px 14px 0 0;
    background: var(--surface-soft);
  }
  header { display: flex; align-items: center; gap: 7px; margin-bottom: 8px; }
  .prompt-icon { display: grid; color: var(--accent-strong); }
  .prompt-label { color: var(--muted); font-size: var(--text-2xs); font-weight: 650; letter-spacing: 0.08em; }
  .prompt-scroll { min-height: 0; overflow-y: auto; overscroll-behavior: contain; }
  .prompt-question :global(.markdown) { color: var(--ink); font-size: var(--text-md); line-height: 1.55; }
  .prompt-text { margin: 0; color: var(--ink-soft); font-family: var(--font-prose); font-size: var(--text-md); line-height: 1.5; }
  .prompt-options { display: grid; gap: 6px; margin-top: 10px; }
  .prompt-option {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    width: 100%;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--surface);
    color: var(--ink);
    cursor: pointer;
    text-align: left;
  }
  .prompt-option:hover:not(:disabled) { border-color: var(--accent); background: var(--accent-soft); }
  .prompt-option:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .prompt-option:disabled { cursor: default; opacity: 0.6; }
  .option-index { display: grid; width: 18px; height: 18px; flex: 0 0 auto; place-items: center; border-radius: 5px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-sm); }
  .option-copy { display: grid; gap: 2px; min-width: 0; }
  .option-copy strong { font-size: var(--text-md); font-weight: 600; line-height: 1.45; }
  .option-copy span { color: var(--muted); font-size: var(--text-sm); line-height: 1.45; }
  .prompt-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 10px; }
  .prompt-hint { margin: 8px 0 0; color: var(--muted); font-size: var(--text-sm); }
</style>
