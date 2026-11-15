<script lang="ts">
  import {
    canConnect,
    manualCommands,
    type ClaudeCodeStatus,
    type ClaudeCodeTransport,
    type ClaudeConnectResult,
  } from "../lib/claudeCode";
  import { errorText } from "../lib/settingsState";

  /**
   * The Claude Code page: what Mnemo2 installed for Claude Code (the
   * mnemo2-mcp helper and the ready plugin folder), what Claude Code itself
   * reports (the CLI, the mnemo marketplace, the installed plugin), and the
   * Connect button that registers the plugin with the `claude` CLI. A machine
   * whose `mnemo` marketplace already points elsewhere (a source checkout) is
   * left alone — Connect disables itself and the page says so — and the
   * manual path (Claude Desktop, or the two commands) works without the CLI.
   */
  let { transport }: { transport: ClaudeCodeTransport } = $props();

  /** The snapshot; null until the first read lands. */
  let status = $state<ClaudeCodeStatus | null>(null);
  let loadError = $state("");
  /** True while Connect runs; the button shows "Connecting…". */
  let busy = $state(false);
  /** What Connect did; null until the first Connect. */
  let result = $state<ClaudeConnectResult | null>(null);
  /** The rejection when the Connect call itself failed. */
  let connectError = $state("");

  $effect(() => {
    let cancelled = false;
    loadError = "";
    transport
      .status()
      .then((loaded) => {
        if (!cancelled) status = loaded;
      })
      .catch((error) => {
        if (!cancelled) loadError = errorText(error);
      });
    return () => {
      cancelled = true;
    };
  });

  /** Re-read the snapshot — Connect changes the marketplace and plugin rows. */
  async function refresh(): Promise<void> {
    try {
      status = await transport.status();
      loadError = "";
    } catch (error) {
      loadError = errorText(error);
    }
  }

  /** Register the plugin with the CLI, then show the new state. */
  async function connect(): Promise<void> {
    busy = true;
    connectError = "";
    try {
      result = await transport.connect();
    } catch (error) {
      connectError = errorText(error);
    } finally {
      await refresh();
      busy = false;
    }
  }

  /** The Plugin row: what Claude Code knows about the mnemo plugin today. */
  function pluginText(current: ClaudeCodeStatus): string {
    if (current.marketplace === "other") {
      return `Claude Code uses the mnemo plugin from ${current.marketplace_path} (a source checkout). Connect leaves it as it is.`;
    }
    if (current.plugin_installed) {
      return current.plugin_version ? `Installed (v${current.plugin_version})` : "Installed";
    }
    return "Not installed";
  }
</script>

<div class="claude-page">
  <div class="claude-scroll">
    {#if loadError}
      <p class="section-error" role="alert">{loadError}</p>
    {:else if status === null}
      <p class="section-note">Checking Claude Code…</p>
    {:else}
      <section class="group" aria-label="Claude Code">
        <header>
          <h3>Claude Code</h3>
          <p class="micro">
            Let Claude Code (the CLI or the Claude Desktop Code tab) lead your Mnemo2 projects. Only the lead uses your
            Claude subscription; the workers run on your own endpoints.
          </p>
        </header>
        <dl class="status-list" aria-label="Claude Code status">
          <div class="status-row">
            <dt>Mnemo helper</dt>
            <dd>{status.helper_path ? `Installed: ${status.helper_path}` : "Not installed yet. Restart Mnemo2, or reinstall it."}</dd>
          </div>
          <div class="status-row">
            <dt>Claude Code CLI</dt>
            <dd>{status.cli_path ? `Found: ${status.cli_path}` : "Not found. Install Claude Code, or add the plugin from Claude Desktop (below)."}</dd>
          </div>
          <div class="status-row">
            <dt>Plugin</dt>
            <dd>{pluginText(status)}</dd>
          </div>
        </dl>
        {#if status.error}
          <p class="section-error" role="alert">{status.error}</p>
        {/if}
        <div class="section-actions">
          {#if connectError}
            <p class="section-error" role="alert">{connectError}</p>
          {/if}
          {#if result}
            {#if result.ok}
              <p class="section-note" role="status">{result.message}</p>
            {:else}
              <p class="section-error" role="alert">{result.message}</p>
            {/if}
          {/if}
          <div class="section-buttons">
            <button
              class="button button-primary"
              disabled={busy || status === null || !canConnect(status)}
              onclick={connect}
            >
              {busy ? "Connecting…" : "Connect Claude Code"}
            </button>
          </div>
        </div>
        {#if result && result.steps.length > 0}
          <ul class="step-list">
            {#each result.steps as step, index (index)}
              <li>
                <code>{step.command}</code>
                <span class="step-state" class:failed={!step.ok}>{step.ok ? "ok" : "failed"}</span>
                {#if step.output}
                  <pre class="step-output">{step.output}</pre>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      <section class="group" aria-label="Without the CLI">
        <header>
          <h4>Without the CLI</h4>
          <p class="micro">
            In Claude Desktop, open the Code tab, choose + then Add plugins, add a marketplace from this folder, then
            install mnemo:
          </p>
        </header>
        <code class="plugin-dir">{status.plugin_dir}</code>
        <p class="micro">Or run:</p>
        <pre class="manual-commands">{manualCommands(status.plugin_dir).join("\n")}</pre>
      </section>
    {/if}
  </div>
</div>

<style>
  .claude-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .claude-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }

  .group { display: grid; gap: 8px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--canvas); }
  .group header { display: grid; gap: 4px; }
  .group h3 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .group h4 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .micro { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }

  .status-list { display: grid; gap: 6px; margin: 0; font-size: var(--text-body); }
  .status-row { display: grid; grid-template-columns: 140px minmax(0, 1fr); gap: 8px; padding: 7px 9px; border: 1px solid var(--line); border-radius: 9px; }
  .status-row dt { color: var(--muted); font-weight: 620; }
  .status-row dd { margin: 0; overflow-wrap: anywhere; }

  .section-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 2px; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; flex: 1; }
  .section-note { margin: 0; color: var(--muted); font-size: var(--text-body); flex: 1; }
  .section-buttons { display: flex; gap: 8px; flex-shrink: 0; }

  .step-list { display: grid; gap: 6px; margin: 0; padding: 0; list-style: none; }
  .step-list li { display: grid; gap: 4px; padding: 8px 10px; border: 1px solid var(--line); border-radius: 9px; font-size: var(--text-body); }
  .step-list code { overflow-wrap: anywhere; }
  .step-state { color: var(--accent); font-size: 0.82em; font-weight: 650; }
  .step-state.failed { color: var(--danger); }
  .step-output {
    margin: 0;
    padding: 7px 9px;
    border-radius: 6px;
    background: var(--surface-soft);
    font-size: 0.86em;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .plugin-dir { overflow-wrap: anywhere; }
  .manual-commands {
    margin: 0;
    padding: 7px 9px;
    border-radius: 6px;
    background: var(--surface-soft);
    font-size: 0.86em;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  @media (max-width: 640px) {
    .status-row { grid-template-columns: 1fr; }
  }
</style>
