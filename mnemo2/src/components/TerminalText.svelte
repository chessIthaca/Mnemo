<script lang="ts">
  import { ansiCss, type AnsiLine } from "../lib/ansi";

  // Rendered inside a <pre>: the markup below stays on one line so no
  // template whitespace leaks into the output.
  /** Terminal lines; `clip` keeps each to one row (the live preview). */
  let { lines, clip = false }: { lines: AnsiLine[]; clip?: boolean } = $props();
</script>

{#each lines as line, index (index)}<span class="term-line" class:clip>{#each line as segment, part (part)}<span style={ansiCss(segment.style) || undefined}>{segment.text}</span>{/each}</span>{/each}

<style>
  .term-line { display: block; min-height: 1lh; }
  .clip { overflow: hidden; text-overflow: ellipsis; white-space: pre; }
</style>
