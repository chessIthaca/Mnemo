<script lang="ts">
  import { CheckCircle2, ChevronRight, LoaderCircle, XCircle } from "@lucide/svelte";
  import { untrack } from "svelte";
  import { toolErrorSummary } from "../../../frontend/src/lib/toolCardPaths";
  import { parseAnsi, stripAnsi, terminalTail } from "../lib/ansi";
  import { app } from "../lib/store";
  import { prettyArguments } from "../lib/toolArguments";
  import { toolSummary } from "../lib/toolSummary";
  import type { LeadEntry } from "../lib/types";
  import ElapsedTime from "./ElapsedTime.svelte";
  import TerminalText from "./TerminalText.svelte";

  type ToolEntry = Extract<LeadEntry, { kind: "tool" }>;

  let { entry }: { entry: ToolEntry } = $props();
  let expanded = $state(false);
  const status = $derived(
    entry.result === null ? "running" : entry.result.success ? "completed" : "failed",
  );
  const statusLabel = $derived(
    status === "running" ? "Running" : status === "completed" ? "Completed" : "Failed",
  );
  const summary = $derived(
    toolSummary(entry.toolName, entry.arguments, (workerId) =>
      $app.workers.find((worker) => worker.id === workerId)?.name ?? workerId,
    ),
  );
  const errorSummary = $derived(
    entry.result && !entry.result.success ? toolErrorSummary(stripAnsi(entry.result.output)) : "",
  );
  /** The live preview's rows: it grows with the output up to this, never shrinks. */
  const LIVE_ROWS = 5;
  const liveLines = $derived(terminalTail(entry.liveOutput, LIVE_ROWS));
  let liveRows = $state(1);
  $effect(() => {
    const rows = liveLines.length;
    untrack(() => {
      if (rows > liveRows) liveRows = rows;
    });
  });
  const argumentsPreview = $derived(prettyArguments(entry.arguments));
</script>

<article class="tool-card" data-status={status} aria-label={`${summary} — ${statusLabel}`}>
  <details class="tool-disclosure" bind:open={expanded}>
    <summary class="tool-heading">
      <span class="tool-icon" data-status={status}>
        {#if status === "running"}
          <span class="tool-spinner"><LoaderCircle size={11} aria-hidden="true" /></span>
        {:else if status === "completed"}
          <CheckCircle2 size={11} aria-hidden="true" />
        {:else}
          <XCircle size={11} aria-hidden="true" />
        {/if}
      </span>
      <span class="tool-copy"><strong title={summary}>{summary}</strong></span>
      <span class="tool-status" data-status={status}>
        {statusLabel}{#if entry.startedAtMs !== undefined}<ElapsedTime startedAtMs={entry.startedAtMs} endedAtMs={entry.endedAtMs} />{/if}
      </span>
      <ChevronRight class="tool-chevron" size={12} aria-hidden="true" />
    </summary>

    {#if expanded}
      <div class="tool-details">
        <div class="tool-identity"><code>{entry.toolName}</code></div>
        {#if entry.arguments}
          <section class="tool-detail-section">
            <h4>Arguments</h4>
            <pre>{argumentsPreview}</pre>
          </section>
        {/if}
        {#if entry.result === null && entry.liveOutput}
          <section class="tool-detail-section">
            <h4>Live output</h4>
            <pre><TerminalText lines={parseAnsi(entry.liveOutput)} /></pre>
          </section>
        {/if}
        {#if entry.result && entry.result.output}
          <section class="tool-detail-section">
            <h4>{entry.result.success ? "Result" : "Error"}</h4>
            <pre><TerminalText lines={parseAnsi(entry.result.output)} /></pre>
          </section>
        {/if}
      </div>
    {/if}
  </details>

  {#if !expanded && status === "running" && entry.liveOutput}
    <pre class="tool-live" aria-live="polite" style:--live-rows={liveRows}><TerminalText lines={liveLines} clip /></pre>
  {/if}
  {#if !expanded && errorSummary}
    <p class="tool-error">{errorSummary}</p>
  {/if}
</article>

<style>
  .tool-card { min-width: 0; padding: 3px 0; }
  .tool-disclosure { min-width: 0; }
  .tool-heading { display: flex; min-width: 0; align-items: center; gap: 8px; border-radius: 4px; cursor: pointer; list-style: none; }
  .tool-heading::-webkit-details-marker { display: none; }
  .tool-heading:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  .tool-icon { display: grid; width: 18px; height: 18px; flex: 0 0 auto; place-items: center; border: 1px solid var(--line-strong); border-radius: 50%; background: var(--canvas); color: var(--muted); }
  .tool-icon[data-status="running"], .tool-icon[data-status="completed"] { color: var(--accent-strong); }
  .tool-icon[data-status="failed"] { color: var(--danger); }
  .tool-copy { flex: 0 1 auto; min-width: 0; }
  .tool-copy strong { display: block; overflow: hidden; color: var(--ink-soft); font-size: var(--text-md); font-weight: 560; line-height: 1.5; text-overflow: ellipsis; white-space: nowrap; }
  .tool-heading:hover .tool-copy strong { color: var(--accent-strong); }
  .tool-status { flex: 0 0 auto; color: var(--muted); font-size: var(--text-xs); }
  .tool-status[data-status="running"] { color: var(--accent-strong); }
  .tool-status[data-status="failed"] { color: var(--danger); }
  .tool-status :global(.elapsed-time) { color: var(--muted); }
  .tool-heading :global(.tool-chevron) { flex: 0 0 auto; margin-left: auto; color: var(--muted); transition: transform 120ms ease; }
  .tool-disclosure[open] .tool-heading :global(.tool-chevron) { transform: rotate(90deg); }
  .tool-details { margin: 6px 0 2px 26px; padding-top: 6px; border-top: 1px solid var(--line); }
  .tool-identity { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; color: var(--muted); font-size: var(--text-xs); }
  .tool-identity code { color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-sm); }
  .tool-detail-section { margin-top: 8px; }
  .tool-detail-section h4 { margin: 0 0 5px; color: var(--muted); font-size: var(--text-xs); font-weight: 600; }
  .tool-detail-section pre, .tool-live { max-height: 240px; margin: 0; overflow: auto; border: 1px solid var(--line); border-radius: 8px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-sm); line-height: 1.6; white-space: pre-wrap; word-break: break-word; }
  .tool-detail-section pre { padding: 10px 12px; }
  /* A fixed-height window on the newest lines: it grows to LIVE_ROWS as output
     arrives and never shrinks, so the conversation below does not jump. */
  .tool-live { box-sizing: content-box; height: calc(var(--live-rows, 1) * 1lh); margin: 4px 0 2px 26px; padding: 6px 10px; overflow: hidden; }
  .tool-error { margin: 4px 0 4px 26px; color: var(--danger); font-size: var(--text-sm); line-height: 1.5; }
  .tool-spinner { display: grid; place-items: center; animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 560px) {
    .tool-copy strong { font-size: var(--text-sm); }
  }
</style>
