<script lang="ts">
  import { onMount } from "svelte";
  import { ArrowRight, FolderOpen, GitBranch, Plus } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import ModelBadge from "./ModelBadge.svelte";

  let notice = $state("");

  function shellOnly(feature: string) {
    notice = `${feature} needs the native shell.`;
  }

  onMount(() => {
    void actions.loadProjects();
  });
</script>

<main class="start-screen">
  <header class="start-nav">
    <a class="brand" href="/" aria-label="Mnemo2 home"><span class="brand-mark">M</span><span>Mnemo2</span></a>
    <span class="phase-label">PHASE 2</span>
  </header>

  <section class="start-main">
    <header class="picker-heading">
      <h1>Projects</h1>
      <span>ONE PROJECT PER INSTANCE</span>
    </header>

    <div class="project-list">
      {#each $app.projects as project (project.id)}
        <button class="project-card" onclick={() => actions.openProject(project.id)}>
          <span class="project-monogram">{project.name.slice(0, 1)}</span>
          <span class="project-info">
            <strong>{project.name}</strong>
            <small>{project.description}</small>
            <span class="project-path">{project.path}</span>
          </span>
          <span class="project-meta">
            {#if project.branch}<span><GitBranch size={11} />{project.branch}</span>{/if}
            {#if project.leadModel}<ModelBadge model={project.leadModel} role="lead" />{/if}
            {#if project.lastSession}<small>{project.lastSession}</small>{/if}
          </span>
          <ArrowRight class="project-arrow" size={17} />
        </button>
      {/each}
      {#if $app.projects.length === 0}
        <p class="empty-projects">No projects registered. Open a folder to begin.</p>
      {/if}
    </div>

    <div class="start-actions">
      <button class="button button-quiet" onclick={() => actions.openFolder()}><FolderOpen size={14} /> Open folder</button>
      <button class="button button-quiet" onclick={() => shellOnly("Project creation")}><Plus size={14} /> New project</button>
    </div>
    {#if notice}<p class="shell-notice" role="status">{notice}</p>{/if}
  </section>
</main>

<style>
  .start-screen { min-height: 100vh; min-height: 100dvh; overflow-y: auto; background: var(--canvas); }
  .start-nav { display: flex; align-items: center; justify-content: space-between; width: min(100%, 860px); margin: 0 auto; padding: 24px 32px; }
  .brand { display: inline-flex; align-items: center; gap: 9px; color: var(--ink); font-size: 12px; font-weight: 680; text-decoration: none; }
  .brand-mark { display: grid; width: 28px; height: 28px; place-items: center; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface); color: var(--accent-strong); font-family: var(--font-prose); font-size: 17px; }
  .phase-label { color: var(--muted); font-size: 7px; font-weight: 650; letter-spacing: 0.13em; }
  .start-main { width: min(100%, 860px); margin: 0 auto; padding: 48px 32px 80px; }
  .picker-heading { display: flex; align-items: baseline; justify-content: space-between; gap: 16px; margin-bottom: 14px; }
  .picker-heading h1 { margin: 0; font-family: var(--font-prose); font-size: 30px; font-weight: 500; letter-spacing: -0.03em; }
  .picker-heading span { color: var(--muted); font-size: 7px; letter-spacing: 0.12em; }
  .project-list { display: grid; gap: 8px; }
  .project-card { display: flex; align-items: center; gap: 12px; width: 100%; min-width: 0; padding: 13px 14px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); color: var(--ink); cursor: pointer; text-align: left; transition: background 150ms ease, border-color 150ms ease; }
  .project-card:hover { border-color: var(--line-strong); background: var(--surface-soft); }
  .project-monogram { display: grid; width: 34px; height: 34px; flex: 0 0 auto; place-items: center; border-radius: 9px; background: var(--surface-soft); color: var(--accent-strong); font-family: var(--font-prose); font-size: 18px; }
  .project-info { min-width: 0; flex: 1; }
  .project-info strong, .project-info small, .project-path { display: block; }
  .project-info strong { font-size: 12px; }
  .project-info small { margin-top: 2px; color: var(--muted); font-size: 9px; }
  .project-path { margin-top: 4px; overflow: hidden; color: #a0a398; font-family: var(--font-mono); font-size: 8px; text-overflow: ellipsis; white-space: nowrap; }
  .project-meta { display: flex; flex: 0 0 auto; flex-direction: column; align-items: flex-end; gap: 3px; text-align: right; }
  .project-meta > span { display: flex; align-items: center; gap: 4px; color: var(--ink-soft); font-family: var(--font-mono); font-size: 8px; }
  .project-meta small { color: var(--muted); font-size: 8px; }
  .project-meta :global(.model-badge) { max-width: 145px; overflow: hidden; text-overflow: ellipsis; }
  .project-card :global(.project-arrow) { color: var(--muted); }
  .start-actions { display: flex; gap: 8px; margin-top: 14px; }
  .shell-notice { color: var(--muted); font-size: 10px; }
  @media (max-width: 560px) {
    .start-nav { padding: 20px; }
    .start-main { padding: 32px 20px 60px; }
    .phase-label { display: none; }
    .project-meta small { display: none; }
    .project-card { gap: 10px; padding: 12px; }
    .project-monogram { width: 30px; height: 30px; font-size: 16px; }
  }
</style>
