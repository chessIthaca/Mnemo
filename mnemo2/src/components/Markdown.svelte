<script lang="ts">
  import { onMount } from "svelte";
  import { renderMarkdown } from "../lib/markdown";

  let { source, onOpenLink }: { source: string; onOpenLink?: (url: string) => void } = $props();
  let container: HTMLDivElement;
  let cachedSource = "";
  let cachedHtml = "";
  const html = $derived.by(() => {
    if (source !== cachedSource) {
      cachedSource = source;
      cachedHtml = renderMarkdown(source);
    }
    return cachedHtml;
  });

  onMount(() => {
    container.addEventListener("click", handleClick);
    return () => container.removeEventListener("click", handleClick);
  });

  function handleClick(event: MouseEvent) {
    const origin = event.target instanceof Element ? event.target : null;
    const copyButton = origin?.closest("button[data-copy-code]");
    if (copyButton) {
      const code = copyButton.closest(".code-block")?.querySelector("code")?.textContent ?? "";
      if (navigator.clipboard) void navigator.clipboard.writeText(code);
      copyButton.textContent = "Copied";
      setTimeout(() => { copyButton.textContent = "Copy"; }, 1200);
      return;
    }

    const anchor = origin?.closest("a");
    const href = anchor?.getAttribute("href");
    if (anchor && href && onOpenLink && /^https?:\/\//i.test(href)) {
      event.preventDefault();
      onOpenLink(href);
    }
  }
</script>

<div class="markdown" bind:this={container}>{@html html}</div>

<style>
  .markdown {
    min-width: 0;
    max-width: 100%;
    overflow-x: auto;
    color: var(--ink);
    font-size: var(--text-body);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }
  .markdown :global(p) { margin: 0 0 10px; }
  .markdown :global(p:last-child) { margin-bottom: 0; }
  .markdown :global(h1), .markdown :global(h2), .markdown :global(h3),
  .markdown :global(h4), .markdown :global(h5), .markdown :global(h6) {
    margin: 22px 0 10px;
    font-family: var(--font-ui);
    font-weight: 680;
    letter-spacing: -0.01em;
    line-height: 1.35;
  }
  .markdown :global(h1) { font-size: var(--text-title); }
  .markdown :global(h2) { font-size: var(--text-title); }
  .markdown :global(h3) { font-size: var(--text-body); }
  .markdown :global(h4) { font-size: var(--text-body); }
  .markdown :global(h5) { font-size: var(--text-body); }
  .markdown :global(h6) { font-size: var(--text-body); color: var(--ink-soft); }
  .markdown :global(h1:first-child), .markdown :global(h2:first-child), .markdown :global(h3:first-child),
  .markdown :global(h4:first-child), .markdown :global(h5:first-child), .markdown :global(h6:first-child) { margin-top: 0; }
  .markdown :global(ul), .markdown :global(ol) { margin: 0 0 12px; padding-left: 22px; }
  .markdown :global(li) { margin: 5px 0; padding-left: 3px; }
  .markdown :global(li > ul), .markdown :global(li > ol) { margin: 5px 0; }
  .markdown :global(li.task-list-item) { list-style: none; }
  .markdown :global(input.task-list-item-checkbox) { margin: 0 7px 0 0; accent-color: var(--accent-strong); vertical-align: -0.08em; }
  .markdown :global(blockquote) { margin: 0 0 16px; padding: 2px 0 2px 14px; border-left: 3px solid var(--line-strong); color: var(--ink-soft); }
  .markdown :global(blockquote p:last-child) { margin-bottom: 0; }
  .markdown :global(hr) { margin: 24px 0; border: 0; border-top: 1px solid var(--line-strong); }
  .markdown :global(a) { color: var(--accent-strong); text-decoration-color: var(--line-strong); text-underline-offset: 3px; }
  .markdown :global(strong) { font-weight: 650; }
  .markdown :global(code) {
    border-radius: 5px;
    background: var(--surface-soft);
    padding: 2px 5px;
    font-family: var(--font-mono);
    font-size: 0.92em;
  }
  .markdown :global(table) { width: 100%; margin: 18px 0 22px; border-collapse: collapse; font-family: var(--font-ui); font-size: var(--text-body); line-height: 1.5; }
  .markdown :global(th), .markdown :global(td) { padding: 10px 12px; border-bottom: 1px solid var(--line); text-align: left; }
  .markdown :global(th) { color: var(--muted); background: var(--surface-soft); font-size: var(--text-body); letter-spacing: 0.06em; text-transform: uppercase; }
  .markdown :global(.code-block) { margin: 20px 0; overflow: hidden; border: 1px solid var(--code-line); border-radius: 12px; background: var(--code-bg); }
  .markdown :global(.code-toolbar) { display: flex; align-items: center; justify-content: space-between; padding: 8px 12px; border-bottom: 1px solid var(--code-head-line); color: var(--code-meta); font-family: var(--font-ui); font-size: var(--text-body); letter-spacing: 0.06em; text-transform: uppercase; }
  .markdown :global(.code-toolbar button) { border: 0; background: transparent; color: var(--code-button); cursor: pointer; font-size: var(--text-body); text-transform: none; }
  .markdown :global(pre) { margin: 0; padding: 16px; overflow-x: auto; }
  .markdown :global(pre code) { padding: 0; background: transparent; color: var(--code-ink); font-size: 0.92em; line-height: 1.65; }
  .markdown :global(.hljs-keyword), .markdown :global(.hljs-built_in) { color: var(--code-keyword); }
  .markdown :global(.hljs-string), .markdown :global(.hljs-title) { color: var(--code-string); }
  .markdown :global(.hljs-number), .markdown :global(.hljs-literal) { color: var(--code-number); }
  .markdown :global(.hljs-comment) { color: var(--code-comment); font-style: italic; }
</style>
