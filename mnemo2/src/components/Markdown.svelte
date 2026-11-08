<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { renderMarkdown } from "../lib/markdown";
  import { repoPathTarget, splitRepoPaths, type RepoPathRef } from "../lib/repoPaths";

  let {
    source,
    onOpenLink,
    onOpenPath,
  }: {
    source: string;
    onOpenLink?: (url: string) => void;
    /** When given, the repository paths the text names become clickable. */
    onOpenPath?: (ref: RepoPathRef) => void;
  } = $props();

  /** Streamed text grows by appends; re-render it at most this often. */
  const STREAM_RENDER_MS = 150;
  let shown = $state(untrack(() => source)); // the source the html was rendered from
  let lastShownAt = 0; // Date.now() of the last `shown` change; 0 so the first change renders at once
  let pending: ReturnType<typeof setTimeout> | null = null;

  function show(next: string): void {
    shown = next;
    lastShownAt = Date.now();
  }

  function cancelPending(): void {
    if (pending !== null) {
      clearTimeout(pending);
      pending = null;
    }
  }

  // A streamed reply appends to its text on every chunk, and re-parsing the
  // whole markdown for each chunk is O(n^2) on the main thread. So appends
  // re-render at most every 150 ms, with the newest text within 150 ms of the
  // last chunk; any other change (a replaced or rewritten text) renders at once.
  $effect(() => {
    const next = source;
    untrack(() => {
      if (next === shown) return;
      const appended = shown.length > 0 && next.startsWith(shown);
      const since = Date.now() - lastShownAt;
      if (!appended || since >= STREAM_RENDER_MS) {
        cancelPending();
        show(next);
        return;
      }
      if (pending === null) {
        pending = setTimeout(() => {
          pending = null;
          show(source);
        }, STREAM_RENDER_MS - since);
      }
    });
  });

  onDestroy(cancelPending);

  let container = $state<HTMLDivElement | null>(null);
  let cachedSource = "";
  let cachedHtml = "";
  const html = $derived.by(() => {
    if (shown !== cachedSource) {
      cachedSource = shown;
      cachedHtml = renderMarkdown(shown);
    }
    return cachedHtml;
  });

  // The rendered HTML is replaced wholesale when the source changes, which
  // takes the path buttons with it: put them back after every render.
  $effect(() => {
    void html;
    decoratePaths();
  });

  onMount(() => {
    container?.addEventListener("click", handleClick);
    return () => container?.removeEventListener("click", handleClick);
  });

  /** Replace each repository path the text names with a focusable button. */
  function decoratePaths(): void {
    const root = container;
    const open = onOpenPath;
    if (!root || !open) return;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode(node) {
        const parent = node.parentElement;
        // Code blocks, real links and these buttons stay exactly as rendered.
        if (!parent || parent.closest("pre, a, button, .md-path")) return NodeFilter.FILTER_REJECT;
        return NodeFilter.FILTER_ACCEPT;
      },
    });
    const textNodes: Text[] = [];
    while (walker.nextNode()) textNodes.push(walker.currentNode as Text);
    for (const node of textNodes) wrapPaths(node, open);
  }

  /** Splice one text node's paths back in as buttons. */
  function wrapPaths(node: Text, open: (ref: RepoPathRef) => void): void {
    const segments = splitRepoPaths(node.data);
    if (segments.every((segment) => segment.ref === null)) return;
    const replacement = document.createDocumentFragment();
    for (const segment of segments) {
      const ref = segment.ref;
      if (!ref) {
        replacement.append(document.createTextNode(segment.text));
        continue;
      }
      const button = document.createElement("button");
      button.type = "button";
      button.className = "md-path";
      button.title = repoPathTarget(ref);
      button.textContent = segment.text;
      button.addEventListener("click", () => open(ref));
      replacement.append(button);
    }
    node.replaceWith(replacement);
  }

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
  .markdown :global(button.md-path) { display: inline; padding: 0; border: 0; background: transparent; color: var(--accent-strong); font: inherit; text-align: left; text-underline-offset: 3px; cursor: pointer; }
  .markdown :global(button.md-path:hover) { text-decoration: underline; }
  .markdown :global(button.md-path:focus-visible) { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 4px; }
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
