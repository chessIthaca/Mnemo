<script lang="ts">
  import { CheckCircle2, LoaderCircle, XCircle } from "@lucide/svelte";
  import type { LeadEntry } from "../lib/types";

  type ToolEntry = Extract<LeadEntry, { kind: "tool" }>;

  let { entry }: { entry: ToolEntry } = $props();
  const status = $derived(
    entry.result === null ? "running" : entry.result.success ? "completed" : "failed",
  );
  const statusLabel = $derived(
    status === "running" ? "Running" : status === "completed" ? "Completed" : "Failed",
  );
</script>

<article class="tool-card" data-status={status} aria-label={`${entry.toolName} — ${statusLabel}`}>
  <div class="tool-heading">
    <span class="tool-icon" data-status={status}>
      {#if status === "running"}
        <span class="tool-spinner"><LoaderCircle size={14} aria-hidden="true" /></span>
      {:else if status === "completed"}
        <CheckCircle2 size={14} aria-hidden="true" />
      {:else}
        <XCircle size={14} aria-hidden="true" />
      {/if}
    </span>
    <div class="tool-copy">
      <strong>{entry.toolName}</strong>
      <span>TOOL CALL · {entry.time}</span>
    </div>
    <span class="tool-status" data-status={status}>{statusLabel}</span>
  </div>

  {#if entry.arguments}
    <details class="tool-detail">
      <summary>Arguments</summary>
      <pre>{entry.arguments}</pre>
    </details>
  {/if}

  {#if entry.result === null && entry.liveOutput}
    <details class="tool-detail" open>
      <summary>Live output</summary>
      <pre>{entry.liveOutput}</pre>
    </details>
  {/if}

  {#if entry.result && entry.result.output}
    <details class="tool-detail">
      <summary>{entry.result.success ? "Result" : "Error"}</summary>
      <pre>{entry.result.output}</pre>
    </details>
  {/if}
</article>

<style>
  .tool-card { margin: 12px 0 16px 30px; padding: 10px 13px; border: 1px solid var(--line); border-radius: 11px; background: var(--surface-soft); }
  .tool-heading { display: flex; align-items: center; gap: 9px; }
  .tool-icon { display: grid; width: 25px; height: 25px; flex: 0 0 auto; place-items: center; border-radius: 7px; background: var(--surface); color: var(--muted); }
  .tool-icon[data-status="running"] { color: var(--accent-strong); }
  .tool-icon[data-status="completed"] { color: var(--accent-strong); }
  .tool-icon[data-status="failed"] { color: var(--danger); }
  .tool-copy { flex: 1; min-width: 0; }
  .tool-copy strong { display: block; overflow: hidden; font-family: var(--font-mono, monospace); font-size: 10px; font-weight: 650; text-overflow: ellipsis; white-space: nowrap; }
  .tool-copy span { display: block; margin-top: 3px; color: var(--muted); font-size: 7px; letter-spacing: 0.07em; }
  .tool-status { flex: 0 0 auto; color: var(--muted); font-size: 8px; font-weight: 650; }
  .tool-status[data-status="running"] { color: var(--accent-strong); }
  .tool-status[data-status="failed"] { color: var(--danger); }
  .tool-detail { margin-top: 8px; border-top: 1px solid var(--line); padding-top: 7px; }
  .tool-detail summary { color: var(--muted); cursor: pointer; font-size: 8px; font-weight: 650; letter-spacing: 0.05em; }
  .tool-detail pre { max-height: 180px; margin: 7px 0 0; overflow: auto; color: var(--ink-soft); font-family: var(--font-mono, monospace); font-size: 9px; line-height: 1.5; white-space: pre-wrap; word-break: break-word; }
  .tool-spinner { display: grid; place-items: center; animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (max-width: 560px) { .tool-card { margin-left: 0; } }
</style>
