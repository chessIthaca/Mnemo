<script lang="ts">
  import { GitBranch as GitBranchIcon, RotateCw } from "@lucide/svelte";
  import { onMount } from "svelte";
  import {
    canMoveMain,
    createGitTransport,
    distanceLabel,
    moveQuestion,
    type GitBranch,
    type GitOverview,
  } from "../lib/git";

  const transport = createGitTransport();

  let overview = $state<GitOverview | null>(null);
  let error = $state("");
  let loading = $state(false);
  let confirming = $state<string | null>(null);
  let moveError = $state("");
  let moving = $state(false);
  let notice = $state("");

  async function load() {
    if (!transport) return;
    loading = true;
    error = "";
    try {
      overview = await transport.overview();
    } catch (err) {
      error = String(err);
      overview = null;
    } finally {
      loading = false;
    }
  }

  function askMove(branch: GitBranch) {
    confirming = branch.name;
    moveError = "";
    notice = "";
  }

  async function move(branch: GitBranch) {
    if (!transport) return;
    moving = true;
    moveError = "";
    try {
      const moved = await transport.moveMain(branch.name);
      notice = `main now points at ${moved.branch} (${moved.commits} commits). Nothing was pushed.`;
      confirming = null;
      await load();
    } catch (err) {
      moveError = String(err);
    } finally {
      moving = false;
    }
  }

  onMount(() => {
    void load();
  });
</script>

<section class="git-tab" aria-label="Git">
  <header class="git-header">
    <div>
      <p>LOCAL REPOSITORY — NOTHING IS PUSHED</p>
      <h2>Git</h2>
    </div>
    <button
      class="button button-quiet"
      aria-label="Refresh the Git view"
      disabled={!transport || loading}
      onclick={() => void load()}
    >
      <RotateCw size={12} /> Refresh
    </button>
  </header>

  {#if !transport}
    <p class="git-note">
      The Git tab reads the desktop app's repository; outside it there is nothing to show.
    </p>
  {:else if error}
    <p class="git-error" role="alert">{error}</p>
  {:else if overview}
    <p class="git-current">
      <GitBranchIcon size={13} />
      {#if overview.branch}
        On <strong>{overview.branch}</strong>
      {:else}
        Detached HEAD
      {/if}
      {#if overview.main}
        <span class="git-separator">·</span> main at <code>{overview.main}</code>
      {:else}
        <span class="git-separator">·</span> no local main branch
      {/if}
    </p>

    {#if notice}
      <p class="git-notice" role="status">{notice}</p>
    {/if}
    {#if overview.main_worktree}
      <p class="git-note">
        main is checked out in <code>{overview.main_worktree}</code>{overview.main_dirty
          ? " with uncommitted changes"
          : ""} — check out another branch there before moving main.
      </p>
    {/if}
    {#if moveError}
      <p class="git-error" role="alert">{moveError}</p>
    {/if}

    <h3 class="git-section">Branches</h3>
    <ul class="branch-list">
      {#each overview.branches as branch (branch.name)}
        <li class="branch-row" class:current={branch.current}>
          <span class="branch-name">{branch.name}</span>
          {#if branch.current}<span class="branch-tag">current</span>{/if}
          <span class="branch-distance">{distanceLabel(branch)}</span>
          {#if canMoveMain(branch, overview.main_worktree)}
            {#if confirming === branch.name}
              <span
                class="branch-confirm"
                role="group"
                aria-label={`Confirm moving main to ${branch.name}`}
              >
                <span class="branch-question">{moveQuestion(branch)}</span>
                <button class="button" disabled={moving} onclick={() => void move(branch)}>
                  Move main
                </button>
                <button
                  class="button button-quiet"
                  disabled={moving}
                  onclick={() => (confirming = null)}
                >
                  Cancel
                </button>
              </span>
            {:else}
              <button class="button button-quiet" onclick={() => askMove(branch)}>
                Move main here
              </button>
            {/if}
          {/if}
        </li>
      {/each}
    </ul>

    <h3 class="git-section">Recent commits</h3>
    <ol class="commit-list">
      {#each overview.commits as commit (commit.id)}
        <li class="commit-row">
          <span class="commit-lane" aria-hidden="true"></span>
          <code class="commit-id">{commit.short}</code>
          <span class="commit-summary">{commit.summary}</span>
          <span class="commit-meta">{commit.author} · {commit.date}</span>
          {#if commit.refs.length}
            <span class="commit-refs">
              {#each commit.refs as ref (ref)}
                <span class="commit-ref">{ref}</span>
              {/each}
            </span>
          {/if}
        </li>
      {/each}
    </ol>
    {#if overview.commits.length === 0}
      <p class="git-note">There are no commits yet.</p>
    {:else if overview.truncated}
      <p class="git-note">Showing the newest 50 commits.</p>
    {/if}
  {:else}
    <p class="git-note">Loading the repository…</p>
  {/if}
</section>

<style>
  .git-tab { display: flex; flex-direction: column; gap: 10px; padding: 14px 16px 20px; }
  .git-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 10px; }
  .git-header p { margin: 0 0 3px; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); letter-spacing: 0.08em; }
  .git-header h2 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 550; }
  .git-current { display: flex; align-items: center; flex-wrap: wrap; gap: 5px; margin: 0; color: var(--ink-soft); font-size: var(--text-body); }
  .git-current strong { color: var(--ink); font-family: var(--font-mono); font-weight: 600; }
  .git-current code { padding: 1px 5px; border-radius: 4px; background: var(--surface-soft); font-family: var(--font-mono); font-size: var(--text-meta); }
  .git-separator { color: var(--muted); }
  .git-note { margin: 0; color: var(--muted); font-size: var(--text-meta); }
  .git-note code { font-family: var(--font-mono); }
  .git-notice { margin: 0; padding: 7px 9px; border: 1px solid var(--accent-line); border-radius: 7px; background: var(--accent-soft); color: var(--ink); font-size: var(--text-meta); }
  .git-error { margin: 0; padding: 7px 9px; border: 1px solid var(--line-strong); border-radius: 7px; background: var(--surface-soft); color: var(--danger); font-family: var(--font-mono); font-size: var(--text-meta); white-space: pre-wrap; }
  .git-section { margin: 6px 0 0; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); font-weight: 600; letter-spacing: 0.08em; text-transform: uppercase; }
  .branch-list { display: flex; flex-direction: column; gap: 2px; margin: 0; padding: 0; list-style: none; }
  .branch-row { display: flex; align-items: center; flex-wrap: wrap; gap: 7px; padding: 5px 8px; border: 1px solid transparent; border-radius: 7px; background: var(--surface-soft); }
  .branch-row.current { border-color: var(--accent-line); }
  .branch-name { color: var(--ink); font-family: var(--font-mono); font-size: var(--text-body); }
  .branch-tag { padding: 0 5px; border: 1px solid var(--accent-line); border-radius: 5px; color: var(--accent-strong); font-family: var(--font-mono); font-size: var(--text-meta); text-transform: uppercase; }
  .branch-distance { color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); }
  .branch-row :global(.button) { margin-left: auto; }
  .branch-confirm { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; margin-left: auto; }
  .branch-question { color: var(--ink-soft); font-size: var(--text-meta); }
  .commit-list { display: flex; flex-direction: column; margin: 0; padding: 0; list-style: none; }
  .commit-row { display: grid; grid-template-columns: 14px auto minmax(0, 1fr); align-items: baseline; gap: 4px 8px; padding: 4px 0; }
  .commit-lane { position: relative; align-self: stretch; justify-self: center; width: 2px; background: var(--line-strong); }
  .commit-lane::before { position: absolute; top: 8px; left: -3px; width: 8px; height: 8px; border-radius: 50%; background: var(--accent); content: ""; }
  .commit-id { color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); }
  .commit-summary { overflow: hidden; color: var(--ink); font-size: var(--text-body); text-overflow: ellipsis; white-space: nowrap; }
  .commit-meta { grid-column: 2 / 4; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); }
  .commit-refs { display: flex; grid-column: 2 / 4; flex-wrap: wrap; gap: 4px; }
  .commit-ref { padding: 0 5px; border: 1px solid var(--line); border-radius: 5px; color: var(--accent-strong); font-family: var(--font-mono); font-size: var(--text-meta); }
</style>
