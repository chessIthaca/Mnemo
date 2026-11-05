<script lang="ts">
  import type { ToolDiffSection } from "../lib/toolDiff";

  /** One change rendered line-by-line in the Diff tab's visual language:
   *  added/removed lines coloured with the app's diff tokens, a file header
   *  per file, height-capped and scrollable like the raw `<pre>` it replaces. */
  let { sections }: { sections: ToolDiffSection[] } = $props();

  const empty = $derived(sections.every((section) => section.lines.length === 0));
</script>

{#if empty}
  <p class="tool-diff-empty">No line changes.</p>
{:else}
  <div class="tool-diff">
    {#each sections as section, sectionIndex (`${sectionIndex}`)}
      {#if section.path !== null || section.note !== null}
        <div class="tool-diff-file">
          {#if section.path !== null}<code>{section.path}</code>{/if}
          {#if section.note !== null}<span>{section.note}</span>{/if}
        </div>
      {/if}
      {#each section.lines as line, index (`${sectionIndex}:${index}`)}
        <div class="tool-diff-line" data-type={line.type}>
          <span class="tool-diff-sign">{line.type === "add" ? "+" : line.type === "remove" ? "−" : line.type === "meta" ? "@" : " "}</span><code>{line.text}</code>
        </div>
      {/each}
    {/each}
  </div>
{/if}

<style>
  .tool-diff { max-height: 320px; margin: 0; overflow: auto; border: 1px solid var(--line); border-radius: 8px; background: var(--surface-soft); padding: 6px 0; font-family: var(--font-mono); font-size: var(--text-body); line-height: 1.6; }
  .tool-diff-line { display: flex; min-width: max-content; padding: 1.5px 12px 1.5px 0; }
  .tool-diff-line code { white-space: pre; }
  .tool-diff-sign { flex: 0 0 24px; color: var(--diff-sign); text-align: center; }
  .tool-diff-line[data-type="add"] { background: var(--diff-add-bg); color: var(--diff-add-ink); }
  .tool-diff-line[data-type="add"] .tool-diff-sign { color: var(--diff-add-sign); }
  .tool-diff-line[data-type="remove"] { background: var(--diff-remove-bg); color: var(--danger-ink); }
  .tool-diff-line[data-type="remove"] .tool-diff-sign { color: var(--diff-remove-sign); }
  .tool-diff-line[data-type="meta"] { margin: 4px 0; background: var(--diff-hunk-bg); color: var(--diff-hunk-ink); }
  .tool-diff-file { display: flex; flex-wrap: wrap; gap: 8px; padding: 6px 12px 3px; }
  .tool-diff-file code { overflow: hidden; color: var(--ink-soft); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .tool-diff-file span { color: var(--muted); font-size: var(--text-meta); }
  .tool-diff-empty { margin: 0; color: var(--muted); font-size: var(--text-body); }
</style>