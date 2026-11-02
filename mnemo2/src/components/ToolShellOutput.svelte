<script lang="ts">
  import { parseAnsi } from "../lib/ansi";
  import TerminalText from "./TerminalText.svelte";

  /** A shell call's split output: stdout as the main block, stderr as its own
   *  red-tinted labelled section. The exit code sits on the parent section's
   *  heading; only the tool's display output renders here. */
  let { stdout, stderr }: { stdout: string; stderr: string | null } = $props();
</script>

{#if stdout !== ""}
  <pre class="tool-shell-pre"><TerminalText lines={parseAnsi(stdout)} /></pre>
{/if}
{#if stderr !== null && stderr !== ""}
  <div class="tool-shell-err">
    <h5>stderr</h5>
    <pre><TerminalText lines={parseAnsi(stderr)} /></pre>
  </div>
{/if}

<style>
  .tool-shell-pre, .tool-shell-err pre { max-height: 240px; margin: 0; overflow: auto; border: 1px solid var(--line); border-radius: 8px; background: var(--surface-soft); color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-body); line-height: 1.6; white-space: pre-wrap; word-break: break-word; }
  .tool-shell-pre { padding: 10px 12px; }
  .tool-shell-err { margin-top: 8px; }
  .tool-shell-err h5 { margin: 0 0 5px; color: var(--danger); font-size: var(--text-meta); font-weight: 600; }
  .tool-shell-err pre { padding: 10px 12px; border-color: var(--danger-line); background: var(--danger-soft); color: var(--danger-ink); }
</style>