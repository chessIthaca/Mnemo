<script lang="ts">
  import { ArrowRight, FolderOpen, GitBranch, Plus, UsersRound } from "@lucide/svelte";
  import { actions, app } from "../lib/store";

  let notice = $state("");
  const previewWorkers = $derived($app.workers.filter((worker) => ["ana", "kai", "ren"].includes(worker.id)));

  function shellOnly(feature: string) {
    notice = `${feature} arrives with the native Mnemo2 shell. Choose a mock project for this prototype.`;
  }
</script>

<main class="start-screen">
  <header class="start-nav">
    <a class="brand" href="/" aria-label="Mnemo2 home"><span class="brand-mark">M</span><span>Mnemo2</span></a>
    <span class="phase-label">PHASE 0 · INTERACTIVE PROTOTYPE</span>
  </header>

  <div class="start-grid">
    <section class="start-main">
      <p class="hero-eyebrow">A LEAD YOU CAN TALK TO. A TEAM YOU CAN SEE.</p>
      <h1>One project.<br />A quieter kind of control.</h1>
      <p class="hero-copy">Plan with the lead, delegate without the log wall, and step in only when a decision actually needs you.</p>

      <div class="recent-heading"><span>Recent projects</span><span>ONE PROJECT PER INSTANCE</span></div>
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
              <span><GitBranch size={11} />{project.branch}</span>
              <small>{project.lastSession}</small>
            </span>
            <ArrowRight class="project-arrow" size={17} />
          </button>
        {/each}
      </div>

      <div class="start-actions">
        <button class="button button-quiet" onclick={() => shellOnly("The folder picker")}><FolderOpen size={14} /> Open folder</button>
        <button class="button button-quiet" onclick={() => shellOnly("Project creation")}><Plus size={14} /> New project</button>
      </div>
      {#if notice}<p class="shell-notice" role="status">{notice}</p>{/if}
    </section>

    <aside class="mission-preview" aria-label="Mission Control preview">
      <div class="preview-top"><span><UsersRound size={14} /> Mission Control</span><span class="live-dot">LIVE MOCK</span></div>
      <div class="preview-summary"><strong>10</strong><span>workers, one plan of record</span><span class="preview-slots">8 slots</span></div>
      <div class="preview-section"><span>NEEDS YOU</span><span>1 review</span></div>
      {#each previewWorkers as worker (worker.id)}
        <div class="preview-worker">
          <span class="avatar" style="--hue: {worker.hue}">{worker.initials}</span>
          <div><strong>{worker.name}</strong><small>{worker.task}</small></div>
          <span class="preview-status" data-status={worker.status}>{worker.status === "in_review" ? "Review" : worker.status === "merged" ? "Merged" : "Working"}</span>
        </div>
      {/each}
      <div class="preview-budget"><span>Project budget</span><strong>$8.64 <small>/ $20</small></strong><div><span></span></div></div>
      <p class="preview-note">Worker tool calls stay in worker threads. Decisions and merge records come back to the lead.</p>
    </aside>
  </div>
</main>

<style>
  .start-screen { min-height: 100vh; min-height: 100dvh; overflow-y: auto; background: radial-gradient(circle at 85% 12%, #ecefe6 0, transparent 34%), var(--canvas); }
  .start-nav { display: flex; align-items: center; justify-content: space-between; max-width: 1240px; margin: 0 auto; padding: 28px 42px; }
  .brand { display: inline-flex; align-items: center; gap: 10px; color: var(--ink); font-size: 13px; font-weight: 680; text-decoration: none; }
  .brand-mark { display: grid; width: 30px; height: 30px; place-items: center; border: 1px solid var(--line-strong); border-radius: 9px; background: var(--surface); color: var(--accent-strong); font-family: var(--font-prose); font-size: 18px; }
  .phase-label { color: var(--muted); font-size: 8px; font-weight: 650; letter-spacing: 0.13em; }
  .start-grid { display: grid; grid-template-columns: minmax(0, 1.35fr) minmax(320px, 0.65fr); gap: clamp(40px, 7vw, 100px); align-items: center; max-width: 1160px; min-height: calc(100vh - 100px); margin: 0 auto; padding: 40px 42px 80px; }
  .hero-eyebrow { margin: 0 0 18px; color: var(--accent-strong); font-size: 9px; font-weight: 700; letter-spacing: 0.14em; }
  h1 { max-width: 650px; margin: 0; font-family: var(--font-prose); font-size: clamp(44px, 5.5vw, 70px); font-weight: 470; line-height: 1.02; letter-spacing: -0.045em; }
  .hero-copy { max-width: 520px; margin: 22px 0 38px; color: var(--muted); font-family: var(--font-prose); font-size: 17px; line-height: 1.65; }
  .recent-heading { display: flex; justify-content: space-between; margin-bottom: 10px; color: var(--ink-soft); font-size: 11px; font-weight: 650; }
  .recent-heading span:last-child { color: var(--muted); font-size: 7px; letter-spacing: 0.12em; }
  .project-list { display: grid; gap: 8px; }
  .project-card { display: flex; align-items: center; gap: 13px; width: 100%; padding: 14px; border: 1px solid var(--line); border-radius: 14px; background: rgb(255 255 255 / 66%); color: var(--ink); cursor: pointer; text-align: left; transition: background 150ms ease, border-color 150ms ease, transform 150ms ease; }
  .project-card:hover { border-color: #c8cec0; background: var(--surface); transform: translateY(-1px); }
  .project-monogram { display: grid; width: 38px; height: 38px; place-items: center; border-radius: 11px; background: var(--surface-soft); color: var(--accent-strong); font-family: var(--font-prose); font-size: 20px; }
  .project-info { flex: 1; min-width: 0; }
  .project-info strong, .project-info small, .project-path { display: block; }
  .project-info strong { font-size: 12px; }
  .project-info small { margin-top: 3px; color: var(--muted); font-size: 9px; }
  .project-path { margin-top: 5px; overflow: hidden; color: #a0a398; font-family: var(--font-mono); font-size: 8px; text-overflow: ellipsis; white-space: nowrap; }
  .project-meta { text-align: right; }
  .project-meta span { display: flex; align-items: center; gap: 4px; color: var(--ink-soft); font-family: var(--font-mono); font-size: 8px; }
  .project-meta small { color: var(--muted); font-size: 8px; }
  .project-card :global(.project-arrow) { color: var(--muted); }
  .start-actions { display: flex; gap: 8px; margin-top: 14px; }
  .shell-notice { color: var(--muted); font-size: 10px; }
  .mission-preview { padding: 20px; border: 1px solid var(--line); border-radius: 20px; background: rgb(255 255 255 / 78%); box-shadow: 0 30px 80px rgb(39 43 34 / 8%); }
  .preview-top { display: flex; justify-content: space-between; color: var(--ink-soft); font-size: 10px; font-weight: 650; }
  .preview-top > span:first-child { display: flex; align-items: center; gap: 6px; }
  .live-dot { color: var(--accent-strong); font-size: 7px; letter-spacing: 0.1em; }
  .preview-summary { display: flex; align-items: baseline; gap: 8px; margin: 24px 0; }
  .preview-summary > strong { font-family: var(--font-prose); font-size: 42px; font-weight: 500; }
  .preview-summary > span { color: var(--muted); font-size: 9px; }
  .preview-summary .preview-slots { margin-left: auto; padding: 4px 7px; border-radius: 6px; background: var(--surface-soft); }
  .preview-section { display: flex; justify-content: space-between; padding: 10px 0; border-top: 1px solid var(--line); color: var(--muted); font-size: 8px; letter-spacing: 0.08em; }
  .preview-worker { display: flex; align-items: center; gap: 10px; padding: 11px 0; border-bottom: 1px solid var(--line); }
  .avatar { display: grid; width: 30px; height: 30px; place-items: center; border-radius: 9px; background: hsl(var(--hue) 32% 88%); color: hsl(var(--hue) 30% 32%); font-size: 9px; font-weight: 750; }
  .preview-worker div { flex: 1; min-width: 0; }
  .preview-worker strong, .preview-worker small { display: block; }
  .preview-worker strong { font-size: 10px; }
  .preview-worker small { overflow: hidden; color: var(--muted); font-size: 8px; text-overflow: ellipsis; white-space: nowrap; }
  .preview-status { color: var(--muted); font-size: 8px; }
  .preview-status[data-status="in_review"] { color: var(--warning); }
  .preview-status[data-status="merged"] { color: var(--accent-strong); }
  .preview-budget { margin-top: 20px; color: var(--muted); font-size: 9px; }
  .preview-budget > strong { float: right; color: var(--ink-soft); font-weight: 550; }
  .preview-budget small { color: var(--muted); font-weight: 400; }
  .preview-budget > div { height: 4px; margin-top: 9px; overflow: hidden; border-radius: 4px; background: var(--surface-soft); }
  .preview-budget > div span { display: block; width: 43%; height: 100%; border-radius: inherit; background: var(--accent); }
  .preview-note { margin: 18px 0 0; color: var(--muted); font-family: var(--font-prose); font-size: 12px; line-height: 1.55; }
  @media (max-width: 900px) { .start-grid { grid-template-columns: 1fr; padding-top: 30px; } .mission-preview { display: none; } }
  @media (max-width: 560px) { .start-nav { padding: 20px; } .phase-label { display: none; } .start-grid { padding: 24px 20px 50px; } .project-meta { display: none; } h1 { font-size: 44px; } }
</style>
