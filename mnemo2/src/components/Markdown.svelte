<script lang="ts">
  import { onMount } from "svelte";

  let { source, onOpenLink }: { source: string; onOpenLink?: (url: string) => void } = $props();
  let html = $state("");
  let container: HTMLDivElement;
  let renderVersion = 0;

  onMount(() => {
    container.addEventListener("click", handleClick);
    return () => container.removeEventListener("click", handleClick);
  });

  $effect(() => {
    const text = source;
    const version = ++renderVersion;
    html = "";
    void import("../lib/markdown").then(({ renderMarkdown }) => {
      if (version === renderVersion) html = renderMarkdown(text);
    });
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

<div class="markdown" bind:this={container}>
  {#if html}
    {@html html}
  {:else}
    <p class="markdown-loading">{source}</p>
  {/if}
</div>

<style>
  .markdown {
    color: var(--ink);
    font-family: var(--font-prose);
    font-size: 16px;
    line-height: 1.72;
    overflow-wrap: anywhere;
  }
  .markdown-loading { white-space: pre-wrap; }
  .markdown :global(p) { margin: 0 0 16px; }
  .markdown :global(p:last-child) { margin-bottom: 0; }
  .markdown :global(h1), .markdown :global(h2), .markdown :global(h3) {
    margin: 28px 0 12px;
    font-family: var(--font-ui);
    font-size: 15px;
    font-weight: 680;
    letter-spacing: -0.01em;
  }
  .markdown :global(h1:first-child), .markdown :global(h2:first-child), .markdown :global(h3:first-child) { margin-top: 0; }
  .markdown :global(ul), .markdown :global(ol) { margin: 0 0 18px; padding-left: 22px; }
  .markdown :global(li) { margin: 5px 0; padding-left: 3px; }
  .markdown :global(a) { color: var(--accent-strong); text-decoration-color: var(--line-strong); text-underline-offset: 3px; }
  .markdown :global(strong) { font-weight: 650; }
  .markdown :global(code) {
    border-radius: 5px;
    background: var(--surface-soft);
    padding: 2px 5px;
    font-family: var(--font-mono);
    font-size: 0.82em;
  }
  .markdown :global(table) { width: 100%; margin: 18px 0 22px; border-collapse: collapse; font-family: var(--font-ui); font-size: 12px; line-height: 1.5; }
  .markdown :global(th), .markdown :global(td) { padding: 10px 12px; border-bottom: 1px solid var(--line); text-align: left; }
  .markdown :global(th) { color: var(--muted); background: var(--surface-soft); font-size: 10px; letter-spacing: 0.06em; text-transform: uppercase; }
  .markdown :global(.code-block) { margin: 20px 0; overflow: hidden; border: 1px solid #2f332e; border-radius: 12px; background: #20231f; }
  .markdown :global(.code-toolbar) { display: flex; align-items: center; justify-content: space-between; padding: 8px 12px; border-bottom: 1px solid #343832; color: #aeb6a7; font-family: var(--font-ui); font-size: 10px; letter-spacing: 0.06em; text-transform: uppercase; }
  .markdown :global(.code-toolbar button) { border: 0; background: transparent; color: #c4ccbd; cursor: pointer; font-size: 10px; text-transform: none; }
  .markdown :global(pre) { margin: 0; padding: 16px; overflow-x: auto; }
  .markdown :global(pre code) { padding: 0; background: transparent; color: #e5e9df; font-size: 12px; line-height: 1.65; }
  .markdown :global(.hljs-keyword), .markdown :global(.hljs-built_in) { color: #c3b5f2; }
  .markdown :global(.hljs-string), .markdown :global(.hljs-title) { color: #b8d59f; }
  .markdown :global(.hljs-number), .markdown :global(.hljs-literal) { color: #e3bc8d; }
  .markdown :global(.hljs-comment) { color: #7f8879; font-style: italic; }
</style>
