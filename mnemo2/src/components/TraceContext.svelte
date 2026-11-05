<script lang="ts">
  // Copyright (c) 2026 Carsten Hess
  // SPDX-License-Identifier: MIT
  // See LICENSE in the repository root.

  import MetricTip from "./MetricTip.svelte";
  import {
    capText,
    messageBlocks,
    prefixReuse,
    prettyJson,
    requestView,
    type TraceDetail,
    type TraceRow,
  } from "../lib/trace";

  /**
   * The Context tab: what the model was asked. The prefix comparison leads
   * (with its honest explanation), request settings read as settings, and
   * the message wall is readable content first, raw JSON one click away.
   */
  let {
    detail,
    previous,
    previousRow,
    previousPending,
  }: { detail: TraceDetail; previous: TraceDetail | null; previousRow: TraceRow | null; previousPending: boolean } =
    $props();

  const view = $derived(requestView(detail));
  const messages = $derived(extractMessages(detail));
  const reuse = $derived(prefixReuse(detail, previous));
  let expanded = $state<Record<number, boolean>>({});

  function extractMessages(source: TraceDetail): unknown[] | null {
    const request = source.request_json;
    if (request === null || typeof request !== "object") return null;
    const list = (request as Record<string, unknown>).messages;
    return Array.isArray(list) ? list : null;
  }

  function comparison(): string | null {
    if (previousRow === null) return null;
    if (previousPending) return null;
    if (reuse === null) return null;
    return `Same beginning as call #${previousRow.id}: ${reuse.sameChars.toLocaleString()} / ${reuse.totalChars.toLocaleString()} characters (${Math.round(reuse.pct * 100)}%) · ${reuse.addedChars.toLocaleString()} new/changed characters`;
  }

  /** A role's chip label. */
  function roleLabel(role: string): string {
    switch (role) {
      case "system":
        return "System";
      case "user":
        return "User";
      case "assistant":
        return "Assistant";
      case "tool":
        return "Tool result";
      default:
        return role;
    }
  }
</script>

<div class="context">
  <section class="block">
    <h3>Prefix reuse <MetricTip term="prefix_reuse" /></h3>
    {#if previousPending}
      <p class="note">Comparing with the previous call…</p>
    {:else if previousRow === null}
      <p class="note">
        No earlier call from this agent with the same purpose and model is loaded, so there is nothing to compare against.
      </p>
    {:else if reuse === null}
      <p class="note">
        The comparison with call #{previousRow.id} is unavailable — one of the two bodies was truncated or not retained
        in full.
      </p>
    {:else}
      <p class="compare">Call #{previousRow.id} shares this beginning: <strong>{reuse.sameChars.toLocaleString()} / {reuse.totalChars.toLocaleString()} characters ({Math.round(reuse.pct * 100)}%)</strong> · {reuse.addedChars.toLocaleString()} new/changed</p>
    {/if}
  </section>

  <section class="block">
    <h3>Request settings</h3>
    {#if view.tools !== null}
      <p class="note">{view.tools.names.length} tool{view.tools.names.length === 1 ? "" : "s"} offered: {view.tools.names.join(", ")}</p>
      <details class="quiet"><summary>Tool schemas</summary><pre>{view.tools.json}</pre></details>
    {:else}
      <p class="note">No tools were offered in this request.</p>
    {/if}
    {#if view.params !== ""}
      <details class="quiet" open={false}>
        <summary>Model parameters</summary>
        <pre>{view.params}</pre>
      </details>
    {/if}
  </section>

  {#if messages === null}
    <section class="block">
      <h3>Messages</h3>
      <p class="note">This request's body did not carry a message list — Raw shows the exact body.</p>
    </section>
  {:else}
    {#if view.system.length > 0}
      <section class="block">
        <details class="quiet">
          <summary>System instructions ({view.system.length})</summary>
          <pre class="system-text">{view.system.join("\n\n")}</pre>
        </details>
      </section>
    {/if}

    <section class="block">
      <h3>Messages ({messages.length})</h3>
      <ol class="message-wall">
        {#each messages as message, index (index)}
          {@const blocks = messageBlocks(message)}
          {@const role = message !== null && typeof message === "object" && "role" in message ? String((message as Record<string, unknown>).role) : "?"}
          {@const capped = capText(blocks.map((block) => block.text).join("\n"))}
          <li class="message role-{roleLabel(role).toLowerCase().replace(/ /g, "-")}">
            <div class="message-head">
              <span class="role-chip">{roleLabel(role)}</span>
              <details class="raw-json">
                <summary>JSON</summary>
                <pre>{prettyJson(message)}</pre>
              </details>
            </div>
            <div class="message-body">
              {#each blocks as block, blockIndex (blockIndex)}
                {#if block.kind === "image"}
                  <span class="image-badge">[image]</span>
                {:else if block.kind === "tool"}
                  <code class="tool-line">{block.text}</code>
                {:else if !capped.capped || expanded[index]}
                  <span class="text-block">{block.text}</span>
                {:else if blockIndex === 0}
                  <span class="text-block">{capped.text}</span>
                {/if}
              {/each}
            </div>
            {#if capped.capped}
              <button
                class="link-button"
                type="button"
                onclick={() => (expanded[index] = true)}
              >Show all {capped.total.toLocaleString()} characters</button>
            {/if}
          </li>
        {/each}
      </ol>
    </section>
  {/if}
</div>

<style>
  .context { display: grid; gap: 16px; }
  .block { display: grid; gap: 8px; }
  .block h3 { display: flex; align-items: center; gap: 6px; margin: 0; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.04em; text-transform: uppercase; }
  .note { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.5; }
  .compare { margin: 0; color: var(--ink-soft); font-size: var(--text-body); line-height: 1.5; }
  .compare strong { color: var(--ink); }
  details.quiet { border: 1px solid var(--line); border-radius: 9px; background: var(--surface-soft); }
  details.quiet summary { padding: 7px 11px; cursor: pointer; color: var(--muted); font-size: var(--text-meta); font-weight: 600; }
  details.quiet summary:hover { color: var(--ink-soft); }
  details.quiet pre { margin: 0; max-height: 340px; overflow: auto; padding: 10px 12px; border-top: 1px solid var(--line); color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-meta); line-height: 1.55; white-space: pre-wrap; overflow-wrap: anywhere; }
  .system-text { margin: 0; max-height: 340px; overflow: auto; padding: 10px 12px; border-top: 1px solid var(--line); color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-meta); line-height: 1.55; white-space: pre-wrap; overflow-wrap: anywhere; }
  .message-wall { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
  .message { display: grid; gap: 6px; padding: 10px 12px; border: 1px solid var(--line); border-radius: 11px; background: var(--surface-soft); }
  .message-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .role-chip { display: inline-flex; padding: 2px 8px; border-radius: 999px; background: var(--surface); border: 1px solid var(--line); color: var(--ink-soft); font-size: var(--text-meta); font-weight: 650; }
  .message.role-assistant .role-chip { color: var(--accent-strong); border-color: var(--accent); }
  details.raw-json { display: inline-flex; }
  details.raw-json summary { cursor: pointer; color: var(--muted); font-size: var(--text-meta); }
  details.raw-json summary:hover { color: var(--ink-soft); }
  details.raw-json pre { position: relative; margin: 8px 0 0; max-height: 300px; overflow: auto; padding: 10px 12px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-meta); line-height: 1.55; white-space: pre-wrap; overflow-wrap: anywhere; }
  .message-body { display: grid; gap: 6px; color: var(--ink); font-size: var(--text-body); line-height: 1.55; }
  .text-block { white-space: pre-wrap; overflow-wrap: anywhere; }
  .image-badge { justify-self: start; padding: 2px 8px; border-radius: 7px; background: var(--surface); border: 1px dashed var(--line-strong); color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); }
  .tool-line { padding: 3px 7px; border-radius: 7px; background: var(--surface); color: var(--accent-strong); font-family: var(--font-mono); font-size: var(--text-meta); overflow-wrap: anywhere; }
  .link-button { justify-self: start; padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: var(--text-meta); font-weight: 600; }
  .link-button:hover { text-decoration: underline; }
</style>