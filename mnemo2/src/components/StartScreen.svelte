<script lang="ts">
  import { onMount } from "svelte";
  import { ArrowRight, FolderOpen, GitBranch, Plus, Settings } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import ModelBadge from "./ModelBadge.svelte";
  import Sheet from "./Sheet.svelte";
  import SettingsDialog from "./SettingsDialog.svelte";

  let projectName = $state("");
  let settingsOpen = $state(false);
  const cleanupProject = $derived($app.projects.find((project) => project.id === $app.cleanupProjectId) ?? null);

  onMount(() => {
    void actions.loadProjects();
  });

  function createProject(event: SubmitEvent) {
    event.preventDefault();
    void actions.createProject(projectName);
  }
</script>

<main class="start-screen">
  <header class="start-nav">
    <a class="brand" href="/" aria-label="Mnemo2 home"><span class="brand-mark">M</span><span>Mnemo2</span></a>
    <div class="start-nav-end">
      <span class="phase-label">LEAD + TEAM</span>
      <button class="icon-button" aria-label="Settings" onclick={() => (settingsOpen = true)}><Settings size={15} /></button>
    </div>
  </header>

  <section class="start-main">
    <header class="picker-heading"><h1>Projects</h1><span>ONE PROJECT PER INSTANCE</span></header>
    <div class="project-list">
      {#each $app.projects as project (project.id)}
        <article class="project-card">
          <button class="project-open" disabled={project.available === false} aria-label={`${project.unfinishedWorkers ? "Resume" : "Open"} ${project.name}`} onclick={() => actions.openProject(project.id)}>
            <span class="project-monogram">{project.name.slice(0, 1)}</span>
            <span class="project-info">
              <strong>{project.name}</strong>
              {#if project.description}<small>{project.description}</small>{/if}
              <span class="project-path">{project.path}</span>
            </span>
            <span class="project-meta">
              {#if project.branch}<span><GitBranch size={11} />{project.branch}</span>{/if}
              {#if project.leadModel}<ModelBadge model={project.leadModel} role="lead" />{/if}
              {#if project.lastSession}<small>{project.lastSession}</small>{/if}
            </span>
            <ArrowRight class="project-arrow" size={17} />
          </button>
          {#if (project.reviewWorkers ?? project.unfinishedWorkers ?? 0) > 0}
            <div class="recovery-actions">
              <span>{project.unfinishedWorkers ? `${project.unfinishedWorkers} unfinished` : "Retained branches"}</span>
              {#if project.unfinishedWorkers}<button class="button button-quiet" onclick={() => actions.openProject(project.id)}>Resume</button>{/if}
              <button class="button button-quiet" onclick={() => actions.reviewProject(project.id)}>Review</button>
              <button class="button button-quiet" onclick={() => actions.requestCleanup(project.id)}>Clean</button>
            </div>
          {/if}
        </article>
      {:else}
        <p class="empty-projects">No projects registered. Open a folder to begin.</p>
      {/each}
    </div>
    <div class="start-actions">
      <button class="button button-quiet" onclick={() => actions.openFolder()}><FolderOpen size={14} /> Open folder</button>
      <button class="button button-quiet" onclick={() => actions.openNewProject()}><Plus size={14} /> New project</button>
    </div>
  </section>
</main>

<Sheet open={$app.projectConflict !== null} title="Project already open" eyebrow="ANOTHER INSTANCE" onclose={actions.cancelProjectConflict}>
  {#if $app.projectConflict}
    <p class="dialog-copy">Another Mnemo instance (PID {$app.projectConflict.pid}) appears to have this project open. Two editors can change the same worktrees. Continue only if you intend to share them.</p>
    <div class="dialog-actions">
      <button class="button button-quiet" onclick={actions.cancelProjectConflict}>Cancel</button>
      <button class="button button-primary" onclick={actions.confirmProjectConflict}>Open anyway</button>
    </div>
  {/if}
</Sheet>

<Sheet open={$app.newProjectParent !== null} title="New project" eyebrow="CREATE LOCALLY" onclose={actions.cancelNewProject}>
  <form class="new-project" onsubmit={createProject}>
    <p class="dialog-copy">Create a folder and a local Git working branch in <code>{$app.newProjectParent}</code>. No remote is created or contacted.</p>
    <label for="project-name">Folder name</label>
    <input id="project-name" bind:value={projectName} maxlength="80" required placeholder="my-project" />
    <div class="dialog-actions">
      <button type="button" class="button button-quiet" onclick={actions.cancelNewProject}>Cancel</button>
      <button type="submit" class="button button-primary" disabled={!projectName.trim() || $app.creatingProject}>{$app.creatingProject ? "Creating…" : "Create project"}</button>
    </div>
  </form>
</Sheet>

<Sheet open={$app.cleanupProjectId !== null} title="Clean inactive worktrees?" eyebrow="PRESERVE EVERY COMMIT" onclose={() => { if (!$app.cleanupBusy) actions.cancelCleanup(); }}>
  <p class="dialog-copy">Only inactive, clean worktrees in <strong>{cleanupProject?.name}</strong> will be removed. Every worker branch is retained. Dirty, active, and conflicting work stays available for review.</p>
  <div class="dialog-actions">
    <button class="button button-quiet" disabled={$app.cleanupBusy} onclick={actions.cancelCleanup}>Cancel</button>
    <button class="button button-primary" disabled={$app.cleanupBusy} onclick={actions.confirmCleanup}>{$app.cleanupBusy ? "Cleaning…" : "Clean worktrees"}</button>
  </div>
</Sheet>

<SettingsDialog open={settingsOpen} onclose={() => (settingsOpen = false)} />

<style>
  .start-screen { min-height: 100vh; min-height: 100dvh; overflow-y: auto; background: var(--canvas); }
  .start-nav { display: flex; align-items: center; justify-content: space-between; width: min(100%, 860px); margin: 0 auto; padding: 24px 32px; }
  .brand { display: inline-flex; align-items: center; gap: 9px; color: var(--ink); font-size: 12px; font-weight: 680; text-decoration: none; }
  .brand-mark, .project-monogram { display: grid; place-items: center; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface); color: var(--accent-strong); font-family: var(--font-prose); }
  .brand-mark { width: 28px; height: 28px; font-size: 17px; }
  .start-nav-end { display: flex; align-items: center; gap: 14px; }
  .phase-label { color: var(--muted); font-size: 8px; font-weight: 650; letter-spacing: 0.13em; }
  .start-main { width: min(100%, 860px); margin: 0 auto; padding: 48px 32px 80px; }
  .picker-heading { display: flex; align-items: baseline; justify-content: space-between; gap: 16px; margin-bottom: 14px; }
  .picker-heading h1 { margin: 0; font-family: var(--font-prose); font-size: 30px; font-weight: 500; letter-spacing: -0.03em; }
  .picker-heading span { color: var(--muted); font-size: 8px; letter-spacing: 0.12em; }
  .project-list { display: grid; gap: 8px; }
  .project-card { overflow: hidden; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
  .project-open { display: flex; align-items: center; gap: 12px; width: 100%; min-width: 0; padding: 13px 14px; border: 0; background: transparent; color: var(--ink); cursor: pointer; text-align: left; }
  .project-open:hover:not(:disabled) { background: var(--surface-soft); }
  .project-open:disabled { opacity: 0.55; cursor: not-allowed; }
  .project-monogram { width: 34px; height: 34px; flex: 0 0 auto; border-radius: 9px; font-size: 18px; }
  .project-info { min-width: 0; flex: 1; }
  .project-info strong, .project-info small, .project-path { display: block; }
  .project-info strong { font-size: 12px; }
  .project-info small { margin-top: 2px; color: var(--muted); font-size: 10px; }
  .project-path { margin-top: 4px; overflow: hidden; color: var(--muted); font-family: var(--font-mono); font-size: 9px; text-overflow: ellipsis; white-space: nowrap; }
  .project-meta { display: flex; flex: 0 0 auto; flex-direction: column; align-items: flex-end; gap: 3px; text-align: right; }
  .project-meta > span { display: flex; align-items: center; gap: 4px; color: var(--ink-soft); font-family: var(--font-mono); font-size: 9px; }
  .project-meta small { color: var(--muted); font-size: 9px; }
  .project-meta :global(.model-badge) { max-width: 145px; overflow: hidden; text-overflow: ellipsis; }
  .project-card :global(.project-arrow) { color: var(--muted); }
  .recovery-actions { display: flex; align-items: center; gap: 6px; padding: 0 12px 10px 60px; }
  .recovery-actions > span { margin-right: auto; color: var(--muted); font-size: 10px; }
  .start-actions { display: flex; gap: 8px; margin-top: 14px; }
  .empty-projects { color: var(--muted); font-size: 12px; line-height: 1.6; }
  .dialog-copy { margin: 0 0 16px; color: var(--muted); font-family: var(--font-prose); font-size: 14px; line-height: 1.65; }
  .dialog-copy code { overflow-wrap: anywhere; font-family: var(--font-mono); font-size: 11px; }
  .dialog-actions { display: flex; justify-content: flex-end; gap: 8px; padding-top: 14px; border-top: 1px solid var(--line); }
  .new-project { display: grid; gap: 10px; }
  .new-project label { color: var(--ink-soft); font-size: 11px; font-weight: 600; }
  .new-project input { padding: 9px 10px; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface); color: var(--ink); font-size: 13px; }
  @media (max-width: 560px) {
    .start-nav { padding: 20px; }
    .start-main { padding: 32px 20px 60px; }
    .phase-label { display: none; }
    .project-meta small { display: none; }
    .project-open { gap: 10px; padding: 12px; }
    .project-monogram { width: 30px; height: 30px; font-size: 16px; }
    .recovery-actions { flex-wrap: wrap; padding-left: 54px; }
    .recovery-actions > span { flex-basis: 100%; }
  }
</style>
