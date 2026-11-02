<script lang="ts">
  import { CheckCircle2, ChevronRight, LoaderCircle, XCircle } from "@lucide/svelte";
  import { untrack } from "svelte";
  import {
    browserResultInfo,
    parseReadFilesSections,
    parseShellOutput,
    searchResultInfo,
    shellCallFromArgs,
    toolErrorSummary,
    webFetchUrl,
  } from "../../../frontend/src/lib/toolCardPaths";
  import { parseAnsi, stripAnsi, terminalTail } from "../lib/ansi";
  import { app } from "../lib/store";
  import { prettyArguments } from "../lib/toolArguments";
  import { editDiffSections, writeDiffSections } from "../lib/toolDiff";
  import { toolHeadline } from "../lib/toolSummary";
  import type { LeadEntry } from "../lib/types";
  import ElapsedTime from "./ElapsedTime.svelte";
  import TerminalText from "./TerminalText.svelte";
  import ToolDiff from "./ToolDiff.svelte";
  import ToolShellOutput from "./ToolShellOutput.svelte";

  type ToolEntry = Extract<LeadEntry, { kind: "tool" }>;

  let { entry }: { entry: ToolEntry } = $props();
  let expanded = $state(false);
  const status = $derived(
    entry.result === null ? "running" : entry.result.success ? "completed" : "failed",
  );
  const statusLabel = $derived(
    status === "running" ? "Running" : status === "completed" ? "Completed" : "Failed",
  );
  const headline = $derived(
    toolHeadline(entry.toolName, entry.arguments, (workerId) =>
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

  // The expanded body, per tool kind — the Mnemo1 CallDetail port: a shell
  // shows its command and its split output, and edits, writes and reads show
  // the change itself (a coloured diff, the written lines, the files and line
  // ranges read) instead of the Arguments JSON and the raw result text. The
  // generic view stays the body for every other tool — and, for the rich
  // rows, stays reachable behind the Show raw call toggle. A failed call
  // never renders a change; its error text is always the result section.

  /** A shell call rendered human-readably: its purpose, its working
   *  directory, and the command, straight from the arguments. */
  const shellCall = $derived(entry.toolName === "shell" ? shellCallFromArgs(entry.arguments) : null);
  /** A completed shell's display output, split into stdout / stderr / exit code. */
  const shellOut = $derived(
    entry.result !== null && entry.toolName === "shell" ? parseShellOutput(entry.result.output) : null,
  );
  /** An edit's diff: the result's structured payload, or — a replayed call
   *  carries none — one rebuilt from the call's own old/new strings. */
  const editDiff = $derived(
    entry.result !== null && (entry.toolName === "file_edit" || entry.toolName === "multi_edit")
      ? editDiffSections(entry.toolName, entry.arguments, entry.result)
      : null,
  );
  /** A write's content as added lines under its path. */
  const writeDiff = $derived(
    entry.result !== null && entry.result.success &&
      (entry.toolName === "file_write" || entry.toolName === "write_file" || entry.toolName === "file_append")
      ? writeDiffSections(entry.toolName, entry.arguments)
      : null,
  );
  /** A successful read's compact what-was-read list, parsed from its output. */
  const readList = $derived(
    entry.result !== null && entry.result.success && entry.toolName === "read_files"
      ? parseReadFilesSections(entry.result.output)
      : [],
  );
  /** Search transparency: which engine served the query and how much it found. */
  const searchInfo = $derived(
    entry.result !== null && entry.result.success &&
      (entry.toolName === "search" || entry.toolName === "search_read")
      ? searchResultInfo(entry.result.output)
      : null,
  );
  /** A browser call's one-line outcome (the landed URL, the page title). */
  const browserInfo = $derived(
    entry.result !== null && entry.result.success
      ? browserResultInfo(entry.toolName, entry.result.output)
      : null,
  );
  /** The full http(s) URL a web_fetch call fetched. */
  const fetchUrl = $derived(entry.toolName === "web_fetch" ? webFetchUrl(entry.toolName, entry.arguments) : null);
  /** True when a rich body replaces the generic Arguments/Result view, which
   *  then moves behind the Show raw call toggle. Other tools (git, team,
   *  search, …) keep the generic view as their body, exactly as before. */
  const rich = $derived(
    shellCall !== null ||
      shellOut !== null ||
      editDiff !== null ||
      writeDiff !== null ||
      readList.length > 0,
  );
  let showRaw = $state(false);
</script>

<article class="tool-card" data-status={status} aria-label={`${headline.text} — ${statusLabel}`}>
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
      <span class="tool-copy"><strong title={headline.title}>{headline.text}</strong></span>
      <span class="tool-status" data-status={status}>
        {statusLabel}{#if entry.startedAtMs !== undefined}<ElapsedTime startedAtMs={entry.startedAtMs} endedAtMs={entry.endedAtMs} />{/if}
      </span>
      <ChevronRight class="tool-chevron" size={12} aria-hidden="true" />
    </summary>

    {#if expanded}
      {#snippet argumentsSection()}
        {#if entry.arguments}
          <section class="tool-detail-section">
            <h4>Arguments</h4>
            <pre>{argumentsPreview}</pre>
          </section>
        {/if}
      {/snippet}
      {#snippet resultSection()}
        {#if entry.result && entry.result.output}
          <section class="tool-detail-section">
            <h4>{entry.result.success ? "Result" : "Error"}</h4>
            <pre><TerminalText lines={parseAnsi(entry.result.output)} /></pre>
          </section>
        {/if}
      {/snippet}
      <div class="tool-details">
        <div class="tool-identity">
          <code>{entry.toolName}</code>
          {#if searchInfo !== null}
            <span class="tool-chip">
              {searchInfo.engine} · {searchInfo.matches}
              {searchInfo.matches === 1 ? "match" : "matches"}{#if searchInfo.files !== null} in {searchInfo.files} {searchInfo.files === 1 ? "file" : "files"}{/if}
            </span>
          {/if}
          {#if browserInfo !== null}<span class="tool-chip">{browserInfo}</span>{/if}
          {#if fetchUrl !== null}<span class="tool-chip">{fetchUrl}</span>{/if}
        </div>
        {#if searchInfo !== null && searchInfo.note !== null}
          <p class="tool-note">note: {searchInfo.note}</p>
        {/if}

        {#if !rich}{@render argumentsSection()}{/if}

        {#if shellCall !== null}
          <section class="tool-detail-section">
            <h4>Command</h4>
            {#if shellCall.purpose}<p class="tool-label">purpose: {shellCall.purpose}</p>{/if}
            {#if shellCall.cwd}<p class="tool-label">cwd: {shellCall.cwd}</p>{/if}
            <pre class="tool-command">{shellCall.command}</pre>
          </section>
        {/if}

        {#if editDiff !== null}
          <section class="tool-detail-section">
            <h4>Diff</h4>
            <ToolDiff sections={editDiff} />
          </section>
        {:else if writeDiff !== null}
          <section class="tool-detail-section">
            <h4>Content</h4>
            <ToolDiff sections={writeDiff} />
          </section>
        {:else if readList.length > 0}
          <section class="tool-detail-section">
            <h4>Files read</h4>
            <ul class="tool-reads">
              {#each readList as item, index (index)}
                <li>
                  <code>{item.path}</code>
                  {#if item.kind === "range"}
                    <span>lines {item.first}–{item.last} of {item.total}</span>
                  {:else}
                    <span>{item.note}</span>
                  {/if}
                </li>
              {/each}
            </ul>
          </section>
        {/if}

        {#if entry.result === null && entry.liveOutput}
          <section class="tool-detail-section">
            <h4>Live output</h4>
            <pre><TerminalText lines={parseAnsi(entry.liveOutput)} /></pre>
          </section>
        {/if}

        {#if shellOut !== null}
          <section class="tool-detail-section">
            <h4>
              {entry.result?.success ? "Result" : "Error"}{#if shellOut.exitCode !== null}
                · <span class="tool-exit" data-ok={shellOut.exitCode === 0}>exit code {shellOut.exitCode}</span>
              {/if}
            </h4>
            <ToolShellOutput stdout={shellOut.stdout} stderr={shellOut.stderr} />
          </section>
        {:else if !rich}
          {@render resultSection()}
        {/if}

        {#if rich}
          <button class="tool-raw-toggle" type="button" aria-expanded={showRaw} onclick={() => (showRaw = !showRaw)}>
            {showRaw ? "Hide raw call" : "Show raw call"}
          </button>
          {#if showRaw}
            {@render argumentsSection()}
            {@render resultSection()}
          {/if}
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
  .tool-copy strong { display: block; overflow: hidden; color: var(--muted); font-size: var(--text-body); font-weight: 500; line-height: 1.5; text-overflow: ellipsis; white-space: nowrap; }
  .tool-heading:hover .tool-copy strong { color: var(--accent-strong); }
  .tool-status { flex: 0 0 auto; color: var(--muted); font-size: var(--text-meta); }
  .tool-status[data-status="running"] { color: var(--accent-strong); }
  .tool-status[data-status="failed"] { color: var(--danger); }
  .tool-status :global(.elapsed-time) { color: var(--muted); }
  .tool-heading :global(.tool-chevron) { flex: 0 0 auto; margin-left: auto; color: var(--muted); transition: transform 120ms ease; }
  .tool-disclosure[open] .tool-heading :global(.tool-chevron) { transform: rotate(90deg); }
  .tool-details { margin: 6px 0 2px 26px; padding-top: 6px; border-top: 1px solid var(--line); }
  .tool-identity { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; color: var(--muted); font-size: var(--text-meta); }
  .tool-identity code { color: var(--ink-soft); font-family: var(--font-mono); font-size: 0.92em; }
  .tool-chip { padding: 1px 8px; border: 1px solid var(--line); border-radius: 999px; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); overflow-wrap: anywhere; }
  .tool-note { margin: 6px 0 0; color: var(--warning); font-size: var(--text-body); line-height: 1.5; }
  .tool-label { margin: 0 0 5px; color: var(--muted); font-size: var(--text-meta); }
  .tool-exit { font-weight: 600; }
  .tool-exit[data-ok="false"] { color: var(--danger); }
  .tool-detail-section { margin-top: 8px; }
  .tool-detail-section h4 { margin: 0 0 5px; color: var(--muted); font-size: var(--text-meta); font-weight: 600; }
  .tool-detail-section pre, .tool-live { max-height: 240px; margin: 0; overflow: auto; border: 1px solid var(--line); border-radius: 8px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-body); line-height: 1.6; white-space: pre-wrap; word-break: break-word; }
  .tool-detail-section pre { padding: 10px 12px; }
  .tool-reads { margin: 0; padding: 0; list-style: none; border: 1px solid var(--line); border-radius: 8px; background: var(--surface-soft); }
  .tool-reads li { display: flex; flex-wrap: wrap; gap: 4px 10px; justify-content: space-between; padding: 6px 12px; }
  .tool-reads li + li { border-top: 1px solid var(--line); }
  .tool-reads code { overflow-wrap: anywhere; color: var(--ink-soft); font-size: var(--text-body); }
  .tool-reads span { color: var(--muted); font-size: var(--text-meta); white-space: nowrap; }
  .tool-raw-toggle { margin-top: 8px; padding: 0; border: none; background: none; color: var(--muted); font-size: var(--text-meta); text-decoration: underline; text-underline-offset: 2px; cursor: pointer; }
  .tool-raw-toggle:hover { color: var(--accent-strong); }
  .tool-raw-toggle:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; border-radius: 4px; }
  /* A fixed-height window on the newest lines: it grows to LIVE_ROWS as output
     arrives and never shrinks, so the conversation below does not jump. */
  .tool-live { box-sizing: content-box; height: calc(var(--live-rows, 1) * 1lh); margin: 4px 0 2px 26px; padding: 6px 10px; overflow: hidden; }
  .tool-error { margin: 4px 0 4px 26px; color: var(--danger); font-size: var(--text-body); line-height: 1.5; }
  .tool-spinner { display: grid; place-items: center; animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>