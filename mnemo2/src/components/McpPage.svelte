<script lang="ts">
  import {
    MCP_SOURCES,
    MCP_TRANSPORTS,
    draftFromServer,
    draftToServer,
    emptyServerDraft,
    serverSummary,
    serversKey,
    shadowedGlobalName,
    statusFor,
    type McpServer,
    type McpServerDraft,
    type McpSettings,
    type McpTestResult,
    type McpTransport,
  } from "../lib/mcpSettings";
  import { errorText } from "../lib/settingsState";

  /**
   * The MCP servers settings page: the global and the open project's
   * `mcp.toml` servers (the project definition winning by name), an
   * add/edit form for stdio and remote entries, and a Test that connects on
   * a throwaway connection — the backend gives up after 20 seconds and kills
   * the process it spawned, so a broken server can never hang the dialog.
   *
   * Saving replaces the whole set; the backend validates it, writes each
   * source to its own file and swaps the live manager, so the next worker
   * dispatched starts with the new servers. The lead's own session was
   * started when the project opened and picks them up at the next project
   * open — the page says so rather than implying a live lead gained tools.
   */
  let { transport }: { transport: McpTransport } = $props();

  /** The snapshot; null until the first read lands. */
  let snapshot = $state<McpSettings | null>(null);
  let loadError = $state("");

  /** The edited list (a working copy of the snapshot's servers). */
  let servers = $state<McpServer[]>([]);
  /** The last saved list — the dirty check and Discard start from it. */
  let original = $state<McpServer[]>([]);
  let saving = $state(false);
  let saveError = $state("");
  let saveNote = $state("");

  /** The add/edit form; closed until Add or Edit opens it. */
  let formOpen = $state(false);
  let editing = $state(false);
  /** The name the edited row had when the form opened (a rename keeps its slot). */
  let editingName = $state("");
  let draft = $state<McpServerDraft>(emptyServerDraft("global"));
  let formError = $state("");

  /** The row whose Test is running / whose result is shown. */
  let testing = $state("");
  let testFor = $state("");
  let testResult = $state<McpTestResult | null>(null);
  let testError = $state("");

  const dirty = $derived(serversKey(servers) !== serversKey(original));

  /** A server copy whose lists and maps the editor may change freely. */
  function cloneServer(server: McpServer): McpServer {
    return {
      ...server,
      args: [...server.args],
      env: [...server.env],
      scopes: [...server.scopes],
      headers_env: { ...server.headers_env },
    };
  }

  /** Adopt a snapshot; unsaved edits stay put. */
  function accept(settings: McpSettings): void {
    snapshot = settings;
    loadError = "";
    if (!dirty) {
      servers = settings.servers.map(cloneServer);
      original = settings.servers.map(cloneServer);
    }
  }

  $effect(() => {
    let cancelled = false;
    loadError = "";
    transport
      .get()
      .then((settings) => {
        if (!cancelled) accept(settings);
      })
      .catch((failure) => {
        if (!cancelled) loadError = errorText(failure);
      });
    return () => {
      cancelled = true;
    };
  });

  function sourceLabel(source: McpServer["source"]): string {
    return MCP_SOURCES.find((entry) => entry.id === source)?.label ?? source;
  }

  /** The row's live status line; a dead lead never reports connections. */
  function statusText(server: McpServer): string {
    if (!snapshot?.live_lead) return "Not connected (no open lead)";
    const status = statusFor(snapshot.statuses, server.name);
    if (!status) return "Not connected yet";
    if (status.connected) {
      const tools = `${status.tool_count} tool${status.tool_count === 1 ? "" : "s"}`;
      return `Connected · ${tools}`;
    }
    return status.last_error ? "Connection failed" : "Not connected";
  }

  function statusDetail(server: McpServer): string {
    if (!snapshot?.live_lead) return "";
    const status = statusFor(snapshot.statuses, server.name);
    return status?.last_error ?? "";
  }

  function beginAdd(): void {
    draft = emptyServerDraft("global");
    editing = false;
    editingName = "";
    formError = "";
    formOpen = true;
  }

  function beginEdit(server: McpServer): void {
    draft = draftFromServer(server);
    editing = true;
    editingName = server.name;
    formError = "";
    formOpen = true;
  }

  function cancelForm(): void {
    formOpen = false;
    formError = "";
  }

  /** Validate the draft and put it in the list; Save writes the list to disk. */
  function submitForm(): void {
    const current = editing ? servers.find((server) => server.name === editingName) : undefined;
    const built = draftToServer(draft, current);
    if (!built.ok) {
      formError = built.error;
      return;
    }
    const others = servers.filter((server) => server.name !== editingName);
    const shadowed = shadowedGlobalName(others, draft);
    if (shadowed) {
      formError = `A project server named '${shadowed}' already exists and would hide this global one — rename it or add it as a project server.`;
      return;
    }
    servers = editing
      ? servers.map((server) => (server.name === editingName ? built.server : server))
      : [...servers, built.server];
    formOpen = false;
    formError = "";
    saveNote = "";
  }

  function removeServer(name: string): void {
    servers = servers.filter((server) => server.name !== name);
    if (testFor === name) {
      testFor = "";
      testResult = null;
      testError = "";
    }
    saveNote = "";
  }

  function toggleEnabled(name: string): void {
    servers = servers.map((server) =>
      server.name === name ? { ...server, enabled: !server.enabled } : server,
    );
    saveNote = "";
  }

  function discard(): void {
    servers = original.map(cloneServer);
    saveError = "";
    saveNote = "";
  }

  async function save(): Promise<void> {
    saveError = "";
    saveNote = "";
    saving = true;
    try {
      const settings = await transport.save(servers);
      original = settings.servers.map(cloneServer);
      servers = settings.servers.map(cloneServer);
      snapshot = settings;
      saveNote = settings.live_lead
        ? "Saved. The next team worker starts with these servers; the lead's own session picks them up at the next project open."
        : "Saved. Applies the next time a project opens.";
    } catch (failure) {
      saveError = errorText(failure);
    } finally {
      saving = false;
    }
  }

  async function runTest(name: string): Promise<void> {
    testing = name;
    testFor = name;
    testResult = null;
    testError = "";
    // Test runs on the SAVED configuration — a row still only in the list
    // has no file entry to connect to yet.
    if (!original.some((server) => server.name === name)) {
      testing = "";
      testError = "Save the list first, then test — Test connects to the saved server.";
      return;
    }
    try {
      testResult = await transport.test(name);
    } catch (failure) {
      testError = errorText(failure);
    } finally {
      testing = "";
    }
  }

  /** The Test line for one row: the tools it advertises or why it failed. */
  function testText(result: McpTestResult): string {
    if (!result.ok) return result.error ?? "The test failed.";
    const names = result.tool_names.join(", ");
    const count = `${result.tool_count} tool${result.tool_count === 1 ? "" : "s"}`;
    return names === "" ? `${count} (none advertised)` : `${count}: ${names}`;
  }
</script>

<div class="mcp-page">
  <div class="mcp-scroll">
    {#if loadError}
      <p class="load-error" role="alert">{loadError}</p>
    {:else if snapshot === null}
      <p class="load-note">Loading…</p>
    {:else}
      <section class="group" aria-label="MCP servers">
        <header>
          <h3>MCP servers</h3>
          <p class="micro">
            Model Context Protocol servers add tools to the lead and its team, named
            <code>mcp__&lt;server&gt;__&lt;tool&gt;</code>. Global servers apply to every project; a project
            server with the same name hides the global one, and removing it reveals that global definition again.
          </p>
        </header>

        {#if servers.length === 0}
          <p class="micro empty">No servers configured yet — add one below.</p>
        {:else}
          <ul class="server-list">
            {#each servers as server (server.name)}
              <li class="server-row" class:off={!server.enabled}>
                <div class="server-head">
                  <span class="source-badge" class:project={server.source === "project"}>
                    {sourceLabel(server.source)}
                  </span>
                  <strong class="server-name">{server.name}</strong>
                  {#if server.trusted}<span class="trusted-badge">Trusted</span>{/if}
                  {#if !server.enabled}<span class="off-badge">Off</span>{/if}
                  <span
                    class="status"
                    class:on={snapshot.live_lead && statusFor(snapshot.statuses, server.name)?.connected === true}
                  >
                    {statusText(server)}
                  </span>
                </div>
                <code class="transport">{serverSummary(server)}</code>
                {#if statusDetail(server)}
                  <p class="status-detail">{statusDetail(server)}</p>
                {/if}
                <div class="row-actions">
                  <label class="enabled-toggle">
                    <input
                      type="checkbox"
                      checked={server.enabled}
                      onchange={() => toggleEnabled(server.name)}
                    />
                    Enabled
                  </label>
                  <button
                    class="button button-quiet"
                    disabled={testing !== ""}
                    onclick={() => runTest(server.name)}
                  >
                    {testing === server.name ? "Testing…" : "Test"}
                  </button>
                  <button class="button button-quiet" onclick={() => beginEdit(server)}>Edit</button>
                  <button class="button button-quiet" onclick={() => removeServer(server.name)}>
                    Remove
                  </button>
                </div>
                {#if testFor === server.name}
                  <p
                    class="test-result"
                    class:failed={testError !== "" || testResult?.ok === false}
                    role="status"
                  >
                    {#if testError}
                      {testError}
                    {:else if testResult}
                      {testText(testResult)}
                    {:else}
                      Connecting…
                    {/if}
                  </p>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}

        <div class="section-actions">
          {#if saveError}<p class="section-error" role="alert">{saveError}</p>{/if}
          {#if saveNote && !dirty}<p class="section-note">{saveNote}</p>{/if}
          <div class="section-buttons">
            <button class="button button-quiet" disabled={saving} onclick={beginAdd}>Add server</button>
            <button class="button button-quiet" disabled={saving || !dirty} onclick={discard}>
              Discard
            </button>
            <button class="button button-primary" disabled={saving || !dirty} onclick={save}>
              {saving ? "Saving…" : "Save servers"}
            </button>
          </div>
        </div>
      </section>

      {#if formOpen}
        <section class="group" aria-label="Server editor">
          <header>
            <h3>{editing ? `Edit ${editingName}` : "Add a server"}</h3>
            <p class="micro">
              {#if editing}
                Changes stay in the list until you press Save servers.
              {:else}
                The new server joins the list; press Save servers to write it, then Test it.
              {/if}
            </p>
          </header>
          <div class="form-grid">
            <label class="field">
              <span>Name</span>
              <input bind:value={draft.name} placeholder="github" autocomplete="off" />
            </label>
            <label class="field">
              <span>Scope</span>
              <select bind:value={draft.source}>
                {#each MCP_SOURCES as source (source.id)}
                  <option
                    value={source.id}
                    disabled={source.id === "project" && !snapshot.project_open}
                  >
                    {source.label}
                  </option>
                {/each}
              </select>
            </label>
            <div class="field field-wide">
              <span>Transport</span>
              <div class="transport-row" role="radiogroup" aria-label="Transport">
                {#each MCP_TRANSPORTS as option (option.id)}
                  <label class="transport-option" class:selected={draft.transport === option.id}>
                    <input
                      type="radio"
                      name="mcp-transport"
                      value={option.id}
                      bind:group={draft.transport}
                    />
                    <span>{option.label}</span>
                  </label>
                {/each}
              </div>
            </div>
            {#if draft.transport === "stdio"}
              <label class="field field-wide">
                <span>Command</span>
                <input bind:value={draft.command} placeholder="npx" autocomplete="off" />
              </label>
              <label class="field field-wide">
                <span>Arguments (one per line)</span>
                <textarea rows="3" bind:value={draft.args} placeholder={"-y\nsome-mcp-server"}></textarea>
              </label>
            {:else}
              <label class="field field-wide">
                <span>URL</span>
                <input bind:value={draft.url} placeholder="https://example.com/mcp" autocomplete="off" />
              </label>
            {/if}
            <label class="check field-wide">
              <input type="checkbox" bind:checked={draft.trusted} />
              Trust this server's tools — they skip the approval prompt when the mode would ask
            </label>
          </div>
          <p class="micro">
            Secrets never live in <code>mcp.toml</code>: <code>env</code> and <code>headers_env</code> name
            environment variables whose values are read at connect time. {#if draft.transport === "remote"}
              OAuth sign-in is not on this page — set <code>auth = "oauth"</code> and <code>client_id</code>
              in <code>mcp.toml</code>.
            {:else}
              OAuth and header settings belong to remote servers; edit <code>mcp.toml</code> for those.
            {/if}
          </p>
          {#if formError}<p class="section-error" role="alert">{formError}</p>{/if}
          <div class="section-actions">
            <div class="section-buttons">
              <button class="button button-quiet" onclick={cancelForm}>Cancel</button>
              <button class="button button-primary" onclick={submitForm}>
                {editing ? "Apply changes" : "Add to list"}
              </button>
            </div>
          </div>
        </section>
      {/if}

      <section class="group" aria-label="When this applies">
        <header>
          <h3>When this applies</h3>
        </header>
        <p class="micro">
          {#if snapshot.live_lead}
            Saving swaps the set the team's engine holds now: the next worker dispatched starts with these
            servers. The lead's own session started when the project opened, so it picks them up at the next
            project open. Test connects right away.
          {:else}
            Saving writes the files the engines read; the changes apply the next time a project opens. Test
            connects right away.
          {/if}
          Test runs on a throwaway connection, gives up after 20 seconds and stops the process it started.
        </p>
      </section>
    {/if}
  </div>
</div>

<style>
  .mcp-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .mcp-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }
  .load-error { margin: 0; padding: 18px 24px; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  .load-note { margin: 0; padding: 18px 24px; color: var(--muted); font-size: var(--text-body); }

  .group { display: grid; gap: 8px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--canvas); }
  .group header { display: grid; gap: 4px; }
  .group h3 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .micro { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }
  .micro code { padding: 1px 4px; border-radius: 5px; background: var(--surface-soft); font-size: 0.92em; }
  .empty { font-style: italic; }

  .server-list { display: grid; gap: 6px; margin: 0; padding: 0; list-style: none; }
  .server-row { display: grid; gap: 6px; padding: 9px 11px; border: 1px solid var(--line); border-radius: 9px; }
  .server-row.off { opacity: 0.66; }
  .server-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 8px; }
  .server-name { font-weight: 620; }
  .source-badge {
    padding: 1px 7px;
    border: 1px solid var(--line);
    border-radius: 999px;
    color: var(--muted);
    font-size: 0.78em;
    font-weight: 650;
  }
  .source-badge.project { border-color: var(--accent); color: var(--accent); }
  .trusted-badge {
    padding: 1px 7px;
    border: 1px solid var(--danger);
    border-radius: 999px;
    color: var(--danger);
    font-size: 0.78em;
    font-weight: 700;
  }
  .off-badge { color: var(--muted); font-size: 0.8em; font-weight: 650; }
  .status { margin-left: auto; color: var(--muted); font-size: 0.82em; }
  .status.on { color: var(--accent); }
  .status-detail { margin: 0; color: var(--danger); font-size: 0.86em; overflow-wrap: anywhere; }
  .transport { color: var(--muted); font-size: 0.92em; overflow-wrap: anywhere; }
  .row-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .row-actions .button { min-height: 30px; padding: 4px 10px; }
  .enabled-toggle { display: flex; align-items: center; gap: 5px; color: var(--muted); font-size: 0.86em; }
  .test-result {
    margin: 0;
    padding: 7px 9px;
    border-left: 3px solid var(--accent);
    border-radius: 6px;
    background: var(--surface-soft);
    font-size: var(--text-body);
    overflow-wrap: anywhere;
  }
  .test-result.failed { border-left-color: var(--danger); color: var(--danger); }

  .form-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; }
  .field { display: grid; gap: 4px; font-size: var(--text-body); }
  .field > span { color: var(--muted); }
  .field-wide { grid-column: 1 / -1; }
  .field input,
  .field select,
  .field textarea {
    min-width: 0;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--surface);
    color: inherit;
    font: inherit;
  }
  .transport-row { display: flex; flex-wrap: wrap; gap: 8px; }
  .transport-option {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    cursor: pointer;
    font-size: var(--text-body);
  }
  .transport-option.selected { border-color: var(--accent); background: var(--accent-soft); }
  .check { display: flex; align-items: center; gap: 7px; font-size: var(--text-body); }

  .section-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 2px; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; flex: 1; }
  .section-note { margin: 0; color: var(--muted); font-size: var(--text-body); flex: 1; }
  .section-buttons { display: flex; gap: 8px; flex-shrink: 0; }

  @media (max-width: 720px) {
    .form-grid { grid-template-columns: 1fr; }
  }
</style>
