<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { Brain, LoaderCircle, Search } from "@lucide/svelte";
  import {
    createMemoryTransport,
    displayTitle,
    embedderLabel,
    formatRecordDate,
    formatScore,
    kindLabel,
    isHit,
    isEngineStarting,
    KIND_CHIPS,
    LIST_PAGE_SIZE,
    mergePage,
    type MemoryDetail,
    type MemoryHit,
    type MemoryOverview,
    type MemoryRow,
  } from "../lib/memoryView";
  import { parseRepoPath, repoPathTarget } from "../lib/repoPaths";
  import { actions } from "../lib/store";
  import ElapsedTime from "./ElapsedTime.svelte";
  import Markdown from "./Markdown.svelte";

  const transport = createMemoryTransport();

  /** How long typing settles before a search runs. */
  const SEARCH_DEBOUNCE_MS = 200;
  /** How many hits the search box asks for. */
  const SEARCH_LIMIT = 40;
  /** How many recall-test rows the Diagnostics box asks for. */
  const RECALL_LIMIT = 8;
  /** How long a retry waits while the project engine is still starting. */
  const ENGINE_RETRY_MS = 250;
  /** How long a load runs before its elapsed counter appears. */
  const ELAPSED_AFTER_MS = 1000;

  /** The three surfaces that wait on their first load. */
  type Surface = "list" | "detail" | "overview";

  /** One surface's first-load state (plan d49ea4a6). */
  interface WaitState {
    /** When the wait began — what the elapsed counter counts from. */
    startedAtMs: number;
    /** True after a second: a fast load never flashes a counter. */
    elapsedVisible: boolean;
    /** True while the backend says the project engine is still starting. */
    engineStarting: boolean;
  }

  function quietWait(): WaitState {
    return { startedAtMs: 0, elapsedVisible: false, engineStarting: false };
  }

  let waits = $state<Record<Surface, WaitState>>({
    list: quietWait(),
    detail: quietWait(),
    overview: quietWait(),
  });
  let loading = $state<Record<Surface, boolean>>({ list: false, detail: false, overview: false });

  /** Every timer the tab armed; cleared when it unmounts. */
  const timers: ReturnType<typeof setTimeout>[] = [];
  /** True once the tab is gone: the engine-start retry loop stops there. */
  let disposed = false;

  let rows = $state<MemoryRow[]>([]);
  let offset = $state(0);
  let hasMore = $state(false);
  let kind = $state("all");
  let listError = $state("");

  let query = $state("");
  let hits = $state<MemoryHit[] | null>(null);
  let searchError = $state("");

  let detail = $state<MemoryDetail | null>(null);
  let detailError = $state("");

  let overview = $state<MemoryOverview | null>(null);
  let overviewError = $state("");
  let recallQuery = $state("");
  let recallHits = $state<MemoryHit[] | null>(null);
  let recallError = $state("");

  /** Turn a rejected command into one readable line (errors stay inline). */
  function errorMessage(value: unknown): string {
    if (typeof value === "string") return value;
    return value instanceof Error ? value.message : "Reading the project's memory failed.";
  }

  /** Start a surface's wait: spinner now, elapsed counter after a second. */
  function beginWait(surface: Surface): void {
    loading[surface] = true;
    waits[surface].startedAtMs = Date.now();
    waits[surface].elapsedVisible = false;
    waits[surface].engineStarting = false;
    timers.push(
      setTimeout(() => {
        waits[surface].elapsedVisible = true;
      }, ELAPSED_AFTER_MS),
    );
  }

  /** End a surface's wait: it loaded, or it failed with a real error. */
  function endWait(surface: Surface): void {
    loading[surface] = false;
    waits[surface].engineStarting = false;
  }

  /** Resolve after `ms` — the gap between two tries at a starting engine. */
  function pause(ms: number): Promise<void> {
    return new Promise((resolve) => {
      timers.push(setTimeout(resolve, ms));
    });
  }

  /**
   * Run one memory read. While the backend says the project engine is still
   * starting (a project open is in flight, so its runtime is not installed
   * yet), the surface keeps its spinner up and the read is retried — the wait
   * is a state, not an error. A real failure still surfaces exactly as before.
   */
  async function readWhenReady<T>(surface: Surface, run: () => Promise<T>): Promise<T> {
    for (;;) {
      try {
        return await run();
      } catch (failure) {
        if (!isEngineStarting(failure)) throw failure;
        waits[surface].engineStarting = true;
        await pause(ENGINE_RETRY_MS);
        if (disposed) throw failure;
      }
    }
  }

  /** One record in the reader, or the error that kept it out. */
  async function selectRecord(id: string): Promise<void> {
    if (!transport) return;
    const api = transport;
    // The newest record's body is part of the tab-open load, so it waits out
    // an engine start like the list does; a later click swaps the body in
    // place, keeping the record already on screen until the new one arrives.
    const first = detail === null;
    if (first) beginWait("detail");
    try {
      detail = await readWhenReady("detail", () => api.read(id));
      detailError = "";
    } catch (failure) {
      detailError = errorMessage(failure);
    } finally {
      if (first) endWait("detail");
    }
  }

  /** The list's first page (reset) or its next page (append). */
  async function loadList(reset: boolean): Promise<void> {
    if (!transport) return;
    const api = transport;
    // Only the first page waits with a spinner of its own; "Load more" keeps
    // the rows it is extending on screen. `untrack`: this runs from the mount
    // effect, and reading `rows` there would make writing it re-run the load.
    const firstPage = reset && untrack(() => rows.length === 0);
    if (firstPage) beginWait("list");
    else loading.list = true;
    try {
      const page = await readWhenReady("list", () =>
        api.list({
          kind,
          offset: reset ? 0 : offset,
          limit: LIST_PAGE_SIZE,
        }),
      );
      rows = reset ? page.rows : mergePage(rows, page);
      offset = page.offset + page.rows.length;
      hasMore = page.has_more;
      listError = "";
      // Never open on an empty reading pane: the newest record is the one the
      // tab is about, and it is what a reviewer wants to see first.
      if (reset && page.rows.length > 0) void selectRecord(page.rows[0].id);
      if (reset) detail = reset && page.rows.length === 0 ? null : detail;
    } catch (failure) {
      listError = errorMessage(failure);
    } finally {
      if (firstPage) endWait("list");
      else loading.list = false;
    }
  }

  /** Swap the type filter and start the list over at page one. */
  function selectKind(value: string): void {
    if (kind === value) return;
    kind = value;
    hits = null;
    query = "";
    searchError = "";
    void loadList(true);
  }

  let searchTimer: ReturnType<typeof setTimeout> | undefined;

  /** Run the text search (immediately on Enter, debounced while typing). */
  async function runSearch(): Promise<void> {
    if (!transport) return;
    const text = query.trim();
    if (text === "") {
      hits = null;
      searchError = "";
      return;
    }
    try {
      hits = await transport.search(text, SEARCH_LIMIT);
      searchError = "";
    } catch (failure) {
      searchError = errorMessage(failure);
    }
  }

  function onSearchInput(): void {
    clearTimeout(searchTimer);
    if (query.trim() === "") {
      hits = null;
      searchError = "";
      return;
    }
    searchTimer = setTimeout(() => void runSearch(), SEARCH_DEBOUNCE_MS);
  }

  function onSearchSubmit(event: SubmitEvent): void {
    event.preventDefault();
    clearTimeout(searchTimer);
    void runSearch();
  }

  /** The Diagnostics card: embedder, counts, index bookkeeping. */
  async function loadOverview(): Promise<void> {
    if (!transport) return;
    const api = transport;
    beginWait("overview");
    try {
      overview = await readWhenReady("overview", () => api.overview());
      overviewError = "";
    } catch (failure) {
      overviewError = errorMessage(failure);
    } finally {
      endWait("overview");
    }
  }

  /** The Diagnostics recall test — the store's own ranking, best first. */
  async function runRecall(): Promise<void> {
    if (!transport) return;
    const text = recallQuery.trim();
    if (text === "") return;
    try {
      recallHits = await transport.recallTest(text, RECALL_LIMIT);
      recallError = "";
    } catch (failure) {
      recallError = errorMessage(failure);
    }
  }

  // The tab mounts only while the panel shows it, so this first read is the
  // tab-open load; no poll — knowledge changes when the agent or the user
  // writes a record, not on a clock.
  $effect(() => {
    if (!transport) return;
    void loadList(true);
    void loadOverview();
  });

  onDestroy(() => {
    disposed = true;
    clearTimeout(searchTimer);
    for (const timer of timers) clearTimeout(timer);
  });

  /** The rows the list renders: search hits when a query ran, else the page. */
  const listed = $derived(hits ?? rows);
  const listIsSearch = $derived(hits !== null);
</script>

<section class="memory-view" aria-label="Memory">
  <header class="memory-header">
    <h1>Memory</h1>
    <p>The project's knowledge records and memories, newest first. Read-only.</p>
  </header>

  {#if transport === null}
    <div class="memory-placeholder">
      <Brain size={22} />
      <p>The Memory tab is available in the desktop app.</p>
    </div>
  {:else}
    <div class="memory-body">
      <aside class="memory-side" aria-label="Records">
        <form class="list-search" role="search" onsubmit={onSearchSubmit}>
          <Search size={13} />
          <input
            type="search"
            placeholder="Search records"
            aria-label="Search records"
            bind:value={query}
            oninput={onSearchInput}
          />
        </form>

        <div class="chip-row" role="group" aria-label="Record type">
          {#each KIND_CHIPS as chip (chip.value)}
            <button
              class="chip"
              class:active={kind === chip.value}
              aria-pressed={kind === chip.value}
              onclick={() => selectKind(chip.value)}
            >{chip.label}</button>
          {/each}
        </div>

        <p class="list-status" role="status">
          {#if listIsSearch}
            {listed.length} matching {listed.length === 1 ? "record" : "records"}
          {:else}
            {rows.length}{hasMore ? "+" : ""} {rows.length === 1 ? "record" : "records"}
          {/if}
        </p>

        {#if listError !== ""}
          <p class="list-error" role="alert">{listError}</p>
        {/if}
        {#if searchError !== ""}
          <p class="list-error" role="alert">{searchError}</p>
        {/if}

        <div class="list-scroll">
          {#if loading.list && listed.length === 0}
            <div class="memory-loading" role="status">
              <span class="spin"><LoaderCircle size={15} /></span>
              <span class="memory-loading-text">
                {waits.list.engineStarting
                  ? "Waiting for the project engine to start…"
                  : "Loading the project's memory…"}
              </span>
              {#if waits.list.elapsedVisible}
                <ElapsedTime startedAtMs={waits.list.startedAtMs} />
              {/if}
            </div>
          {:else if listed.length === 0}
            <p class="list-empty">
              {listIsSearch ? "No record matches that text." : "No record matches this filter."}
            </p>
          {/if}
          <ul class="record-list">
            {#each listed as row (row.id)}
              <li>
                <button
                  class="record-row"
                  class:active={detail?.id === row.id}
                  aria-current={detail?.id === row.id ? "true" : undefined}
                  onclick={() => void selectRecord(row.id)}
                >
                  <span class="record-top">
                    <span class="badge" data-kind={row.kind}>{kindLabel(row.kind)}</span>
                    <span class="record-date">{formatRecordDate(row.created_at)}</span>
                    {#if isHit(row)}<span class="record-score">{formatScore(row.score)}</span>{/if}
                  </span>
                  <span class="record-title">{displayTitle(row.title)}</span>
                  <span class="record-preview">{row.preview}</span>
                </button>
              </li>
            {/each}
          </ul>
          {#if !listIsSearch && hasMore}
            <button class="load-more" disabled={loading.list} onclick={() => void loadList(false)}>
              {loading.list ? "Loading …" : "Load more"}
            </button>
          {/if}
        </div>

        <details class="diagnostics">
          <summary>Diagnostics</summary>
          <div class="diagnostics-body">
            {#if overviewError !== ""}
              <p class="list-error" role="alert">{overviewError}</p>
            {:else if overview === null}
              <div class="memory-loading" role="status">
                <span class="spin"><LoaderCircle size={13} /></span>
                <span class="memory-loading-text">
                  {waits.overview.engineStarting
                    ? "Waiting for the project engine to start…"
                    : "Loading the project's memory…"}
                </span>
                {#if waits.overview.elapsedVisible}
                  <ElapsedTime startedAtMs={waits.overview.startedAtMs} />
                {/if}
              </div>
            {:else if !overview.available}
              <p class="diag-quiet">{overview.reason}</p>
            {:else}
              <dl class="diag-grid">
                <dt>Embedder</dt>
                <dd>{overview.model_id} · {overview.dim} dims · {embedderLabel(overview.embedder)}</dd>
                <dt>Configured</dt>
                <dd>{overview.configured_model ?? "hash (built-in)"}</dd>
                <dt>Vectors</dt>
                <dd>
                  {#if overview.fingerprints.length === 0}
                    none stored yet
                  {:else}
                    {#each overview.fingerprints as fingerprint (`${fingerprint.model}:${fingerprint.dim}`)}
                      {fingerprint.model} · {fingerprint.dim}
                    {/each}
                    {#if !overview.fingerprint_matches}<span class="diag-warn">model changed — re-embeds at the next start</span>{/if}
                  {/if}
                </dd>
                <dt>Tiers</dt>
                <dd>
                  {overview.counts.working} working · {overview.counts.episodic} episodic ·
                  {overview.counts.semantic} semantic · {overview.counts.procedural} procedural
                </dd>
                <dt>Index</dt>
                <dd>
                  {#if overview.index}
                    {overview.index.authored_count} authored · {overview.index.derived_count} derived ·
                    built {formatRecordDate(overview.index.last_built_at)}
                  {:else}
                    not built yet
                  {/if}
                </dd>
              </dl>

              <form
                class="recall-box"
                onsubmit={(event) => {
                  event.preventDefault();
                  void runRecall();
                }}
              >
                <label for="recall-query">Recall test</label>
                <div class="recall-input">
                  <input id="recall-query" type="search" placeholder="Ask the store" bind:value={recallQuery} />
                  <button class="button button-quiet" type="submit" disabled={recallQuery.trim() === ""}>Recall</button>
                </div>
              </form>
              {#if recallError !== ""}
                <p class="list-error" role="alert">{recallError}</p>
              {:else if recallHits !== null}
                {#if recallHits.length === 0}
                  <p class="diag-quiet">Nothing matched.</p>
                {:else}
                  <ol class="recall-list">
                    {#each recallHits as hit (hit.id)}
                      <li>
                        <button class="recall-hit" onclick={() => void selectRecord(hit.id)}>
                          <span class="badge" data-kind={hit.kind}>{kindLabel(hit.kind)}</span>
                          <span class="recall-title">{displayTitle(hit.title)}</span>
                          <span class="record-score">{formatScore(hit.score)}</span>
                        </button>
                      </li>
                    {/each}
                  </ol>
                {/if}
              {/if}
            {/if}
          </div>
        </details>
      </aside>

      <div class="memory-reader">
        {#if detailError !== ""}
          <div class="memory-placeholder">
            <p role="alert">{detailError}</p>
          </div>
        {:else if detail === null && loading.detail}
          <div class="memory-placeholder" role="status">
            <span class="spin"><LoaderCircle size={22} /></span>
            <p>
              {waits.detail.engineStarting
                ? "Waiting for the project engine to start…"
                : "Loading the project's memory…"}
            </p>
            {#if waits.detail.elapsedVisible}
              <ElapsedTime startedAtMs={waits.detail.startedAtMs} />
            {/if}
          </div>
        {:else if detail === null}
          <div class="memory-placeholder">
            <Brain size={22} />
            <p>Pick a record to read it.</p>
          </div>
        {:else}
          <article class="record" aria-label="Record">
            <header class="record-head">
              <p class="record-kicker">
                <span class="badge" data-kind={detail.kind}>{kindLabel(detail.kind)}</span>
                <span class="record-tier">{detail.tier}</span>
                {#if detail.status === "superseded"}
                  <span class="badge warn">SUPERSEDED</span>
                {/if}
                {#if detail.quarantined}
                  <span class="badge warn" title={detail.quarantined}>QUARANTINED</span>
                {/if}
                {#if detail.unconfirmed}
                  <span class="badge" title="The record does not say how it is known">UNCONFIRMED</span>
                {/if}
              </p>
              <h2>{displayTitle(detail.title)}</h2>
              <p class="record-meta">
                {#if detail.path}
                  {@const pathRef = parseRepoPath(detail.path)}
                  {#if pathRef}
                    <button class="path-link" title={repoPathTarget(pathRef)} onclick={() => actions.openRepoPath(pathRef)}><code>{detail.path}</code></button>
                  {:else}
                    <code>{detail.path}</code>
                  {/if}
                {/if}
                <span>{detail.created ?? formatRecordDate(detail.created_at)}</span>
                <span>{detail.access_count} {detail.access_count === 1 ? "recall" : "recalls"}</span>
              </p>
              {#if detail.evidence}
                <p class="record-evidence">Evidence: {detail.evidence}</p>
              {/if}
            </header>

            <Markdown source={detail.markdown} onOpenPath={actions.openRepoPath} />

            {#if detail.links.length > 0}
              <section class="record-links" aria-label="Links">
                <h3>Links</h3>
                <ul>
                  {#each detail.links as link (link)}
                    {@const linkRef = parseRepoPath(link)}
                    <li>
                      {#if linkRef}
                        <button class="path-link" title={repoPathTarget(linkRef)} onclick={() => actions.openRepoPath(linkRef)}><code>{link}</code></button>
                      {:else}
                        <code>{link}</code>
                      {/if}
                    </li>
                  {/each}
                </ul>
              </section>
            {/if}

            {#if detail.backlinks.length > 0}
              <section class="record-links" aria-label="Backlinks">
                <h3>Backlinks</h3>
                <ul>
                  {#each detail.backlinks as back (back.id)}
                    <li>
                      <button class="backlink" onclick={() => void selectRecord(back.id)}>
                        <span class="badge" data-kind={back.kind}>{kindLabel(back.kind)}</span>
                        {displayTitle(back.title)}
                      </button>
                    </li>
                  {/each}
                </ul>
              </section>
            {/if}
          </article>
        {/if}
      </div>
    </div>
  {/if}
</section>

<style>
  .memory-view { display: flex; flex: 1; min-width: 0; min-height: 0; flex-direction: column; container: memory-view / inline-size; background: var(--canvas); }
  .memory-header { padding: 18px 24px 12px; }
  .memory-header h1 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 500; letter-spacing: -0.03em; }
  .memory-header p { margin: 6px 0 0; color: var(--muted); font-size: var(--text-body); }

  .memory-placeholder { display: grid; flex: 1; place-content: center; justify-items: center; gap: 10px; color: var(--muted); }
  .memory-placeholder p { margin: 0; font-size: var(--text-body); }

  .memory-body { display: grid; flex: 1; min-height: 0; grid-template-columns: 340px minmax(0, 1fr); border-top: 1px solid var(--line); }
  .memory-side { display: flex; min-height: 0; flex-direction: column; border-right: 1px solid var(--line); background: var(--surface); }

  .list-search { display: flex; align-items: center; gap: 7px; margin: 12px 12px 0; padding: 6px 9px; border: 1px solid var(--line); border-radius: 9px; background: var(--canvas); color: var(--muted); }
  .list-search input { width: 100%; min-width: 0; border: 0; outline: none; background: transparent; color: var(--ink); font-size: var(--text-body); }

  .chip-row { display: flex; flex-wrap: wrap; gap: 5px; padding: 10px 12px 0; }
  .chip { padding: 3px 8px; border: 1px solid var(--line); border-radius: 999px; background: var(--surface); color: var(--ink-soft); font-size: var(--text-meta); font-weight: 620; text-transform: uppercase; letter-spacing: 0.04em; cursor: pointer; }
  .chip:hover { border-color: var(--line-strong); color: var(--ink); }
  .chip.active { border-color: var(--accent); background: var(--accent-soft); color: var(--accent-strong); }

  .list-status { margin: 10px 12px 0; color: var(--muted); font-size: var(--text-meta); text-transform: uppercase; letter-spacing: 0.05em; }
  .list-error { margin: 8px 12px 0; color: var(--danger); font-size: var(--text-body); }
  .list-empty { margin: 10px 12px; color: var(--muted); font-size: var(--text-body); }

  /* The first-load wait (plan d49ea4a6): a spinner, what it is waiting for,
     and — after a second — how long it has been waiting. */
  .memory-loading { display: flex; align-items: center; gap: 7px; margin: 10px 12px; color: var(--muted); font-size: var(--text-body); }
  .memory-loading-text { color: var(--ink-soft); }
  .spin { display: inline-flex; animation: spin 0.9s linear infinite; }
  @keyframes spin { from { rotate: 0deg; } to { rotate: 360deg; } }

  .list-scroll { flex: 1; min-height: 0; overflow-y: auto; padding: 8px 0 12px; }
  .record-list { margin: 0; padding: 0; list-style: none; }
  .record-row { display: flex; width: 100%; flex-direction: column; gap: 3px; padding: 9px 12px; border: 0; border-left: 2px solid transparent; background: transparent; text-align: left; cursor: pointer; }
  .record-row:hover { background: var(--surface-soft); }
  .record-row.active { border-left-color: var(--accent); background: var(--accent-soft); }
  .record-top { display: flex; align-items: center; gap: 6px; }
  .badge { padding: 1px 6px; border-radius: 5px; background: var(--surface-soft); color: var(--ink-soft); font-size: var(--text-meta); font-weight: 660; letter-spacing: 0.05em; }
  .badge[data-kind="spec"] { background: var(--accent-soft); color: var(--accent-strong); }
  .badge[data-kind="decision"] { background: var(--badge-decision-bg); color: var(--badge-decision-ink); }
  .badge[data-kind="bug"] { background: var(--badge-bug-bg); color: var(--badge-bug-ink); }
  .badge[data-kind="how"] { background: var(--warning-soft); color: var(--badge-how-ink); }
  .badge.warn { background: var(--warning-soft); color: var(--warning); }
  .record-date { margin-left: auto; color: var(--muted); font-size: var(--text-meta); }
  .record-score { color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); }
  .record-title { overflow: hidden; color: var(--ink); font-size: var(--text-body); font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
  .record-preview { display: -webkit-box; overflow: hidden; color: var(--ink-soft); font-size: var(--text-meta); line-height: 1.45; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; }
  .load-more { display: block; margin: 10px auto 0; padding: 6px 12px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); color: var(--ink-soft); font-size: var(--text-body); cursor: pointer; }
  .load-more:hover { border-color: var(--line-strong); color: var(--ink); }

  .diagnostics { border-top: 1px solid var(--line); background: var(--canvas); }
  .diagnostics summary { padding: 9px 12px; color: var(--ink-soft); font-size: var(--text-meta); font-weight: 660; text-transform: uppercase; letter-spacing: 0.05em; cursor: pointer; }
  .diagnostics-body { max-height: 40vh; overflow-y: auto; padding: 0 12px 12px; }
  .diag-grid { display: grid; margin: 0; gap: 4px 8px; grid-template-columns: 74px minmax(0, 1fr); font-size: var(--text-meta); }
  .diag-grid dt { color: var(--muted); text-transform: uppercase; letter-spacing: 0.04em; }
  .diag-grid dd { margin: 0; color: var(--ink-soft); overflow-wrap: anywhere; }
  .diag-quiet { margin: 0 0 8px; color: var(--muted); font-size: var(--text-meta); }
  .diag-warn { color: var(--warning); }
  .recall-box { margin-top: 12px; }
  .recall-box label { display: block; margin-bottom: 4px; color: var(--muted); font-size: var(--text-meta); text-transform: uppercase; letter-spacing: 0.04em; }
  .recall-input { display: flex; gap: 6px; }
  .recall-input input { min-width: 0; flex: 1; padding: 5px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink); font-size: var(--text-body); }
  .recall-input .button { min-height: 28px; padding: 4px 10px; font-size: var(--text-meta); }
  .recall-list { margin: 8px 0 0; padding: 0 0 0 18px; }
  .recall-hit { display: flex; width: 100%; align-items: center; gap: 6px; padding: 3px 0; border: 0; background: transparent; color: var(--ink); font-size: var(--text-meta); text-align: left; cursor: pointer; }
  .recall-hit:hover .recall-title { text-decoration: underline; }
  .recall-title { overflow: hidden; flex: 1; text-overflow: ellipsis; white-space: nowrap; }

  .memory-reader { min-width: 0; overflow-y: auto; background: var(--surface); }
  .record { max-width: 760px; padding: 22px 30px 40px; }
  .record-head { padding-bottom: 14px; border-bottom: 1px solid var(--line); }
  .record-kicker { display: flex; align-items: center; gap: 8px; margin: 0; }
  .record-tier { color: var(--muted); font-size: var(--text-meta); text-transform: uppercase; letter-spacing: 0.05em; }
  .record-head h2 { margin: 8px 0 6px; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 500; letter-spacing: -0.02em; }
  .record-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin: 0; color: var(--muted); font-size: var(--text-meta); }
  .record-meta code { padding: 1px 5px; border-radius: 4px; background: var(--surface-soft); font-family: var(--font-mono); font-size: var(--text-meta); }
.record-evidence { margin: 0; color: var(--muted); font-size: var(--text-meta); }
  .record :global(.markdown) { font-size: var(--text-body); }
  .record-links { margin-top: 22px; padding-top: 12px; border-top: 1px solid var(--line); }
  .record-links h3 { margin: 0 0 6px; color: var(--muted); font-size: var(--text-meta); font-weight: 660; text-transform: uppercase; letter-spacing: 0.05em; }
  .record-links ul { margin: 0; padding: 0; list-style: none; }
  .record-links li { margin: 3px 0; }
  .record-links code { padding: 1px 5px; border-radius: 4px; background: var(--surface-soft); font-family: var(--font-mono); font-size: var(--text-meta); }
  .path-link { display: inline-flex; padding: 0; border: 0; background: transparent; font: inherit; text-align: left; cursor: pointer; }
  .path-link code { color: var(--accent-strong); }
  .path-link:hover code { text-decoration: underline; }
  .path-link:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 5px; }
  .backlink { display: inline-flex; align-items: center; gap: 6px; padding: 0; border: 0; background: transparent; color: var(--accent-strong); font-size: var(--text-body); text-align: left; cursor: pointer; }
  .backlink:hover { text-decoration: underline; }
  @container memory-view (max-width: 820px) {
    .memory-body { grid-template-columns: 270px minmax(0, 1fr); }
    .record { padding: 18px 20px 32px; }
  }
  @container memory-view (max-width: 580px) {
    .memory-body { grid-template-columns: minmax(0, 1fr); grid-template-rows: minmax(200px, 42%) minmax(0, 1fr); }
    .memory-side { border-right: 0; border-bottom: 1px solid var(--line); }
  }
</style>
