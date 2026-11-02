<script lang="ts">
  // Copyright (c) 2026 Carsten Hess
  // SPDX-License-Identifier: MIT
  // See LICENSE in the repository root.

  import Markdown from "./Markdown.svelte";
  import { responseView, type TraceDetail, type TraceRow } from "../lib/trace";

  /**
   * The Response tab: Reasoning and Answer as separate cards, the streamed
   * text reassembled into readable form. An absent body says so — never an
   * empty card pretending content existed.
   */
  let { row, detail }: { row: TraceRow; detail: TraceDetail } = $props();

  const view = $derived(responseView(detail));
  const streaming = $derived(!row.is_complete && !view.absent);
</script>

<div class="response">
  {#if view.absent}
    <div class="absent">
      {#if row.retained}
        <p class="note"><strong>Retained metadata.</strong> This call survived a restart without its prompt or response. Enable full-body logging for future calls.</p>
      {:else if row.response_evicted}
        <p class="note"><strong>Response evicted.</strong> The recorded response text was dropped from memory to keep the log's budget; the metadata survived.</p>
      {:else}
        <p class="note"><strong>No response recorded.</strong> The call never produced a response body to keep.</p>
      {/if}
    </div>
  {:else}
    {#if streaming}
      <p class="stream-chip">Still streaming — this is text so far.</p>
    {/if}

    {#if view.errorText !== null}
      <section class="card error">
        <h3>Provider error</h3>
        <p>{view.errorText}</p>
      </section>
    {/if}

    {#if view.reasoning !== ""}
      <section class="card">
        <h3>Reasoning</h3>
        <div class="raw-text">{view.reasoning}</div>
      </section>
    {/if}

    {#if view.answer !== ""}
      <section class="card">
        <h3>Answer</h3>
        <div class="answer"><Markdown source={view.answer} /></div>
      </section>
    {:else if view.reasoning === ""}
      <section class="card">
        <h3>Answer</h3>
        <p class="note">
          This call produced no answer text.
          {#if detail.raw_tool_calls !== null && detail.raw_tool_calls.length > 0}It requested {detail.raw_tool_calls.length} tool{detail.raw_tool_calls.length === 1 ? "" : "s"} instead — see Tools.{/if}
        </p>
      </section>
    {/if}

    {#if row.is_complete && row.usage === null}
      <p class="note quiet">The provider did not report token usage for this call.</p>
    {/if}
  {/if}
</div>

<style>
  .response { display: grid; gap: 14px; }
  .card { display: grid; gap: 8px; padding: 13px 15px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface-soft); }
  .card h3 { display: flex; align-items: center; gap: 6px; margin: 0; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.04em; text-transform: uppercase; }
  .card.error { border-color: var(--danger); background: var(--danger-bg); }
  .card.error h3 { color: var(--danger); }
  .card.error p { margin: 0; color: var(--danger-ink); line-height: 1.5; }
  .raw-text { color: var(--ink-soft); font-size: var(--text-body); line-height: 1.6; white-space: pre-wrap; overflow-wrap: anywhere; }
  .answer :global(p) { margin: 0 0 10px; }
  .answer :global(p:last-child) { margin-bottom: 0; }
  .answer :global(pre) { overflow: auto; }
  .note { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.5; }
  .note strong { color: var(--ink); }
  .note.quiet { font-size: var(--text-meta); }
  .absent { display: grid; gap: 8px; padding: 13px 15px; border: 1px dashed var(--line-strong); border-radius: 12px; background: var(--surface-soft); }
  .stream-chip { justify-self: start; margin: 0; padding: 3px 10px; border-radius: 999px; background: var(--running-bg); color: var(--running-ink); font-size: var(--text-meta); font-weight: 650; }
</style>