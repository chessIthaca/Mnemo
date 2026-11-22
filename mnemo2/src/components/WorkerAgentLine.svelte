<script lang="ts">
  import { formatAgo } from "../lib/elapsed";
  import type { WorkerAgent } from "../lib/types";

  /**
   * A worker that runs as a Claude Code agent: where its transcript is and
   * how it is doing, read from that transcript. Mnemo follows the agent
   * through the file, so these facts are all it has.
   */
  let { agent }: { agent: WorkerAgent } = $props();
</script>

<div class="agent-line" aria-label="Claude Code agent">
  <span class="agent-title">Claude Code agent</span>
  {#if agent.stalled}<span class="stalled-badge" title="Nothing written for ten minutes">stalled</span>{/if}
  {#if agent.finished}<span class="finished-badge">finished</span>{/if}
  <span class="agent-activity">
    {#if agent.lastActivityMs != null}last activity {formatAgo(agent.lastActivityMs)}{:else}no activity yet{/if}
    · {agent.toolCalls} tool calls · {agent.tokens.toLocaleString("en-US")} tokens
  </span>
  <code class="agent-path" title={agent.transcript ?? undefined}>{agent.transcript ?? "transcript not found yet"}</code>
</div>

<style>
  .agent-line { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; padding: 6px 12px; border-bottom: 1px solid var(--line); color: var(--muted); font-size: var(--text-meta); }
  .agent-title { font-weight: 650; color: var(--ink-soft); }
  .stalled-badge { padding: 2px 6px; border-radius: 999px; background: var(--danger); color: var(--on-danger); font-weight: 650; text-transform: uppercase; }
  .finished-badge { padding: 2px 6px; border-radius: 999px; border: 1px solid var(--line); }
  .agent-path { min-width: 0; max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
