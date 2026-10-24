<script lang="ts">
  import { ChevronDown, MessageSquare, UsersRound, Wallet } from "@lucide/svelte";
  import { actions, app, currentProject, needsYouCount } from "../lib/store";

  const visibleWorkers = $derived($app.workers.filter((worker) => ["working", "in_review", "fixing"].includes(worker.status)).slice(0, 4));
  const activeCount = $derived($app.workers.filter((worker) => ["working", "in_review", "fixing", "merging"].includes(worker.status)).length);
</script>

<header class="topbar">
  <div class="topbar-left">
    <span class="brand-mark">M</span>
    <button class="project-switch" onclick={actions.closeProject} title="Return to recent projects">
      <strong>{$currentProject?.name ?? "Mnemo2"}</strong>
      <span>{$currentProject?.branch ?? "No project"}</span>
      <ChevronDown size={12} />
    </button>
  </div>

  <nav class="primary-nav" aria-label="Workspace">
    <button class:active={$app.route === "lead"} onclick={actions.openLead}><MessageSquare size={14} /> Lead</button>
    <button class:active={$app.route === "team"} onclick={actions.openTeam}><UsersRound size={14} /> Team
      {#if $needsYouCount > 0}<span class="nav-badge">{$needsYouCount}</span>{/if}
    </button>
  </nav>

  <div class="topbar-right">
    <button class="team-stack" onclick={actions.openTeam} title="Open Mission Control">
      <span class="stack-avatars">
        {#each visibleWorkers as worker (worker.id)}<span class="avatar" style="--hue: {worker.hue}">{worker.initials}</span>{/each}
      </span>
      <span>{activeCount} active</span>
    </button>
    <span class="budget-chip"><Wallet size={13} /><span>${$currentProject?.budgetSpent.toFixed(2) ?? "0.00"}</span><small>/ ${$currentProject?.budgetLimit ?? 0}</small></span>
  </div>
</header>

<style>
  .topbar { position: relative; z-index: 20; display: grid; grid-template-columns: 1fr auto 1fr; align-items: center; min-height: 64px; padding: 0 22px; border-bottom: 1px solid var(--line); background: rgb(247 247 244 / 88%); backdrop-filter: blur(18px); }
  .topbar-left, .topbar-right { display: flex; align-items: center; gap: 14px; }
  .topbar-right { justify-content: flex-end; }
  .brand-mark { display: grid; width: 30px; height: 30px; place-items: center; border: 1px solid var(--line-strong); border-radius: 9px; background: var(--surface); color: var(--accent-strong); font-family: var(--font-prose); font-size: 18px; }
  .project-switch { display: grid; grid-template-columns: 1fr auto; column-gap: 8px; padding: 0; border: 0; background: transparent; color: var(--ink); cursor: pointer; text-align: left; }
  .project-switch strong { font-size: 11px; font-weight: 650; }
  .project-switch span { grid-row: 2; color: var(--muted); font-family: var(--font-mono); font-size: 8px; }
  .project-switch :global(svg) { grid-row: 1 / 3; grid-column: 2; align-self: center; color: var(--muted); }
  .primary-nav { display: flex; gap: 3px; padding: 3px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface-soft); }
  .primary-nav button { display: inline-flex; align-items: center; gap: 6px; padding: 6px 12px; border: 0; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; font-size: 10px; font-weight: 600; }
  .primary-nav button.active { background: var(--surface); color: var(--ink); box-shadow: 0 1px 4px rgb(35 38 30 / 8%); }
  .nav-badge { display: grid; min-width: 15px; height: 15px; place-items: center; border-radius: 8px; background: var(--warning-soft); color: var(--warning); font-size: 8px; }
  .team-stack { display: flex; align-items: center; gap: 8px; padding: 0; border: 0; background: transparent; color: var(--muted); cursor: pointer; font-size: 9px; }
  .stack-avatars { display: flex; padding-left: 8px; }
  .avatar { display: grid; width: 25px; height: 25px; margin-left: -8px; place-items: center; border: 2px solid var(--canvas); border-radius: 50%; background: hsl(var(--hue) 32% 85%); color: hsl(var(--hue) 30% 32%); font-size: 7px; font-weight: 750; }
  .budget-chip { display: inline-flex; align-items: center; gap: 5px; padding: 6px 9px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink-soft); font-size: 9px; }
  .budget-chip small { color: var(--muted); }
  @media (max-width: 760px) { .topbar { grid-template-columns: 1fr auto; padding: 0 14px; } .primary-nav { position: fixed; right: 14px; bottom: 14px; left: 14px; z-index: 40; justify-content: center; box-shadow: var(--shadow); } .primary-nav button { flex: 1; justify-content: center; } .team-stack { display: none; } .budget-chip small { display: none; } }
</style>
