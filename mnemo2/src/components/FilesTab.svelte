<script lang="ts">
  import { ArrowLeft, File as FileIcon, Folder, FolderOpen, RotateCw, Search, X } from "@lucide/svelte";
  import { onMount } from "svelte";
  import {
    createFilesTransport,
    formatBytes,
    highlightFile,
    visibleRows,
    type FileContent,
    type FileEntry,
  } from "../lib/files";

  const transport = createFilesTransport();

  let listings = $state<Record<string, FileEntry[]>>({});
  let expanded = $state<Record<string, boolean>>({});
  let loading = $state<Record<string, boolean>>({});
  let truncatedDirs = $state<Record<string, boolean>>({});
  let treeError = $state("");

  let openFile = $state<FileContent | null>(null);
  let openError = $state("");
  let reading = $state(false);

  let query = $state("");
  let matches = $state<FileEntry[]>([]);
  let searching = $state(false);
  let searchError = $state("");

  /** Flatten a rejected invoke (string or Error) to display text. */
  const errorText = (error: unknown) =>
    typeof error === "string"
      ? error
      : error instanceof Error
        ? error.message
        : "Could not read the project";

  const rows = $derived(visibleRows(listings, expanded));
  const highlighted = $derived(
    openFile && !openFile.binary ? highlightFile(openFile.text, openFile.path) : "",
  );
  const filtering = $derived(query.trim() !== "");

  /** Fetch one directory, once: the tree asks on expand, never upfront. */
  async function loadDir(path: string) {
    const active = transport;
    if (!active || loading[path] || listings[path]) return;
    loading = { ...loading, [path]: true };
    try {
      const listing = await active.list(path);
      listings = { ...listings, [path]: listing.entries };
      truncatedDirs = { ...truncatedDirs, [path]: listing.truncated };
      treeError = "";
    } catch (error) {
      treeError = errorText(error);
    } finally {
      loading = { ...loading, [path]: false };
    }
  }

  function toggleDir(entry: FileEntry) {
    const opening = expanded[entry.path] !== true;
    expanded = { ...expanded, [entry.path]: opening };
    if (opening) void loadDir(entry.path);
  }

  async function openPath(path: string) {
    const active = transport;
    if (!active) return;
    reading = true;
    openError = "";
    try {
      openFile = await active.read(path);
    } catch (error) {
      openFile = null;
      openError = errorText(error);
    } finally {
      reading = false;
    }
  }

  /** Select a row from either the tree or the filtered list. */
  function select(entry: FileEntry) {
    if (entry.kind === "dir") {
      query = "";
      matches = [];
      expanded = { ...expanded, [entry.path]: true };
      void loadDir(entry.path);
      return;
    }
    void openPath(entry.path);
  }

  function closeFile() {
    openFile = null;
    openError = "";
  }

  let searchTimer: ReturnType<typeof setTimeout> | null = null;

  async function runSearch(term: string) {
    const active = transport;
    if (!active) return;
    searching = true;
    try {
      matches = await active.find(term);
      searchError = "";
    } catch (error) {
      matches = [];
      searchError = errorText(error);
    } finally {
      searching = false;
    }
  }

  function onFilter(event: Event) {
    query = (event.currentTarget as HTMLInputElement).value;
    if (searchTimer) clearTimeout(searchTimer);
    const term = query.trim();
    if (term === "") {
      matches = [];
      searching = false;
      searchError = "";
      return;
    }
    searchTimer = setTimeout(() => void runSearch(term), 180);
  }

  function clearFilter() {
    if (searchTimer) clearTimeout(searchTimer);
    query = "";
    matches = [];
    searching = false;
    searchError = "";
  }

  /** Drop every cached listing and read the project root again. */
  async function refresh() {
    listings = {};
    expanded = {};
    truncatedDirs = {};
    treeError = "";
    openFile = null;
    openError = "";
    await loadDir("");
  }

  onMount(() => {
    void loadDir("");
    return () => {
      if (searchTimer) clearTimeout(searchTimer);
    };
  });
</script>

<section class="files-tab" aria-label="Files">
  <header class="files-header">
    <div>
      <p>PROJECT · READ-ONLY</p>
      <h2>Files</h2>
    </div>
    <button class="button button-quiet" aria-label="Refresh files" disabled={!transport} onclick={() => void refresh()}>
      <RotateCw size={12} /> Refresh
    </button>
  </header>

  {#if !transport}
    <p class="files-note">The Files tab needs the desktop app.</p>
  {:else}
    <div class="files-controls">
      <span class="search-icon"><Search size={12} /></span>
      <input
        aria-label="Filter paths"
        placeholder="Filter paths…"
        value={query}
        oninput={onFilter}
      />
      {#if filtering}
        <button class="button button-quiet" aria-label="Clear the path filter" onclick={clearFilter}><X size={12} /></button>
      {/if}
    </div>

    {#if treeError}<div class="files-error" role="alert">{treeError}</div>{/if}
    {#if searchError}<div class="files-error" role="alert">{searchError}</div>{/if}

    {#if openFile || openError || reading}
      <div class="viewer">
        <div class="viewer-head">
          <button class="button button-quiet" aria-label="Back to the file tree" onclick={closeFile}><ArrowLeft size={12} /> Files</button>
          <code>{openFile?.path ?? ""}</code>
          {#if openFile}<span>{formatBytes(openFile.bytes)}</span>{/if}
        </div>
        {#if reading}
          <p class="files-note" role="status">Reading the file…</p>
        {:else if openError}
          <div class="files-error" role="alert">{openError}</div>
        {:else if openFile?.binary}
          <p class="files-note">Binary file · the viewer shows text only.</p>
        {:else if openFile}
          {#if openFile.truncated}
            <p class="files-note">Showing the first {formatBytes(256 * 1024)} of {formatBytes(openFile.bytes)}.</p>
          {/if}
          <pre class="code" aria-label={`Contents of ${openFile.path}`}><code>{@html highlighted}</code></pre>
        {/if}
      </div>
    {:else if filtering}
      <ul class="tree" aria-label="Matching paths">
        {#each matches as entry (entry.path)}
          <li>
            <button class="row" onclick={() => select(entry)} title={entry.path}>
              <FileIcon size={12} />
              <span class="row-path">{entry.path}</span>
            </button>
          </li>
        {/each}
        {#if searching}<li><p class="files-note" role="status">Searching…</p></li>{/if}
        {#if !searching && matches.length === 0}
          <li><p class="files-note">No path matches “{query.trim()}”.</p></li>
        {/if}
      </ul>
    {:else}
      <ul class="tree" aria-label="Project files">
        {#each rows as row (row.entry.path)}
          <li>
            <button
              class="row"
              data-kind={row.entry.kind}
              style={`padding-left: ${8 + row.depth * 14}px`}
              aria-expanded={row.entry.kind === "dir" ? row.expanded : undefined}
              title={row.entry.path}
              onclick={() => (row.entry.kind === "dir" ? toggleDir(row.entry) : void openPath(row.entry.path))}
            >
              {#if row.entry.kind === "dir"}
                {#if row.expanded}<FolderOpen size={12} />{:else}<Folder size={12} />{/if}
              {:else}
                <FileIcon size={12} />
              {/if}
              <span class="row-name">{row.entry.name}</span>
            </button>
            {#if row.entry.kind === "dir" && row.expanded && loading[row.entry.path]}
              <p class="files-note indent" role="status">Loading…</p>
            {/if}
            {#if row.entry.kind === "dir" && row.expanded && truncatedDirs[row.entry.path]}
              <p class="files-note indent">This folder has more entries than the tree shows.</p>
            {/if}
          </li>
        {/each}
        {#if rows.length === 0}
          <li><p class="files-note" role="status">Reading the project…</p></li>
        {/if}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .files-tab { padding: 18px; }
  .files-header { display: flex; align-items: flex-end; justify-content: space-between; gap: 12px; margin-bottom: 14px; }
  .files-header p { margin: 0 0 4px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.11em; }
  .files-header h2 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 550; }
  .files-controls { display: flex; align-items: center; gap: 6px; margin-bottom: 12px; padding: 0 9px; border: 1px solid var(--line); border-radius: 7px; background: var(--surface); }
  .files-controls input { flex: 1; min-width: 0; padding: 7px 0; border: 0; background: transparent; color: var(--ink-soft); font-size: var(--text-body); }
  .files-controls input:focus { outline: none; }
  .search-icon { display: flex; color: var(--muted); }
  .tree { margin: 0; padding: 0; list-style: none; }
  .row { display: flex; align-items: center; gap: 6px; width: 100%; padding: 4px 8px; border: 0; border-radius: 6px; background: transparent; color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-body); text-align: left; }
  .row:hover { background: var(--surface-soft); }
  .row:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .row-path, .row-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .viewer { display: flex; min-width: 0; flex-direction: column; }
  .viewer-head { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
  .viewer-head code { flex: 1; overflow: hidden; color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .viewer-head span { color: var(--muted); font-size: var(--text-meta); white-space: nowrap; }
  .code { max-height: 62vh; margin: 0; overflow: auto; padding: 10px 12px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); font-family: var(--font-mono); font-size: var(--text-meta); line-height: 1.55; tab-size: 2; }
  .code code { white-space: pre; }
  .files-note { margin: 10px 2px; color: var(--muted); font-size: var(--text-body); line-height: 1.6; }
  .files-note.indent { margin: 2px 0 6px 22px; }
  .files-error { margin-bottom: 12px; padding: 10px 12px; border: 1px solid var(--danger-line); border-radius: 8px; background: var(--danger-soft); color: var(--danger); font-size: var(--text-body); line-height: 1.5; }
  @media (max-width: 480px) { .files-tab { padding: 14px; } }
</style>
