<script lang="ts">
  import { ChevronDown, MessageSquare, PanelsTopLeft, UsersRound, Wallet } from "@lucide/svelte";
  import { actions, app, currentProject, needsYouCount } from "../lib/store";
  import AvatarStack from "./AvatarStack.svelte";
  import LeadModelPicker from "./LeadModelPicker.svelte";
  import Sheet from "./Sheet.svelte";

  const visibleWorkers = $derived($app.workers.filter((worker) => ["working", "in_review", "fixing"].includes(worker.status)));
  const activeCount = $derived($app.workers.filter((worker) => ["working", "in_review", "fixing", "merging"].includes(worker.status)).length);
  const tokenUsage = $derived($app.workers.reduce((total, worker) => total + worker.tokens, 0));
  const teamCost = $derived.by(() => {
    let total = 0;
    let hasPricing = false;
    for (const worker of $app.workers) {
      if (worker.cost == null) continue;
      hasPricing = true;
      total += worker.cost;
    }
    return hasPricing ? total : null;
  });
  let settingsOpen = $state(false);

  function handleTeam() {
    if ($app.route === "lead") actions.toggleTeamPeek();
    else actions.openTeam();
  }

  function setSlots(event: Event) {
    const slots = (event.currentTarget as HTMLInputElement).valueAsNumber;
    if (Number.isFinite(slots)) actions.setWorkerSlots(slots);
  }
</script>

<header class="topbar">
  <div class="topbar-left">
    <span class="brand-mark">M</span>
    <button class="project-switch" onclick={actions.closeProject} title="Return to recent projects">
      <strong>{$currentProject?.name ?? "Mnemo2"}</strong>
      <span>{$currentProject ? ($currentProject.branch || "Not a Git checkout") : "No project"}</span>
      <ChevronDown size={12} />
    </button>
  </div>

  <nav class="primary-nav" aria-label="Workspace">
    <button class:active={$app.route === "lead"} aria-pressed={$app.route === "lead"} onclick={actions.openLead}><MessageSquare size={14} /> Lead</button>
    <button class:active={$app.route === "team"} aria-expanded={$app.route === "lead" ? $app.teamPeekOpen : undefined} onclick={handleTeam}><UsersRound size={14} /> Team
      {#if $needsYouCount > 0}<span class="nav-badge">{$needsYouCount}</span>{/if}
    </button>
  </nav>

  <div class="topbar-right">
    <button class="team-stack" onclick={handleTeam} title="Peek at the team or open Mission Control">
      <AvatarStack workers={visibleWorkers} max={4} />
      <span>{activeCount} active</span>
    </button>
    {#if $app.route === "lead"}
      <button
        class="icon-button"
        aria-label={$app.workbenchOpen ? "Close workbench" : "Open workbench"}
        aria-pressed={$app.workbenchOpen}
        onclick={() => ($app.workbenchOpen ? actions.closeWorkbench() : actions.openWorkbench("browser"))}
      ><PanelsTopLeft size={15} /></button>
    {/if}
    <button class="budget-chip" aria-label="Open project settings" title="Cumulative worker usage and project settings" onclick={() => (settingsOpen = true)}>
      <Wallet size={13} /><span>{#if teamCost != null}${teamCost.toFixed(2)}{:else}{tokenUsage} tokens{/if}</span>
    </button>
  </div>
</header>

<Sheet open={settingsOpen} title="Project settings" eyebrow="PER-PROJECT" onclose={() => (settingsOpen = false)}>
  <p class="settings-copy">Worker slots persist per project. Usage is telemetry, not a spending limit.</p>
  <div class="settings-row">
    <div><strong>Worker slots</strong><small>Maximum concurrent workers for this project.</small></div>
    <input aria-label="Project worker slots" type="number" min="1" max="32" step="1" value={$currentProject?.workerSlots ?? 1} onchange={setSlots} />
  </div>
  <div class="settings-row"><div><strong>Lead model</strong><small>Planning and review</small></div><LeadModelPicker compact /></div>
  <div class="settings-row"><div><strong>Merge policy</strong><small>Checks and review must close on the merge-time tip; merges are serialized into this project’s working branch.</small></div><span class="settings-value settings-ok">Guarded</span></div>
  <div class="settings-actions">
    <button class="button button-primary" onclick={() => (settingsOpen = false)}>Done</button>
  </div>
</Sheet>

<Sheet open={$app.closeProjectPending} title="Close project and stop the team?" eyebrow="ACTIVE SESSIONS" onclose={actions.cancelCloseProject}>
  <p class="settings-copy">The lead and any live workers will stop. Branches, worktrees, and transcripts remain available for review.</p>
  <div class="settings-actions">
    <button class="button button-quiet" onclick={actions.cancelCloseProject}>Keep working</button>
    <button class="button button-primary" onclick={actions.confirmCloseProject}>Stop and close</button>
  </div>
</Sheet>

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
  .budget-chip { display: inline-flex; align-items: center; gap: 5px; padding: 6px 9px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: 9px; }
  .budget-chip:hover { border-color: var(--line-strong); }
  .settings-copy { margin: 0 0 16px; color: var(--muted); font-family: var(--font-prose); font-size: 14px; line-height: 1.55; }
  .settings-row { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 13px 0; border-top: 1px solid var(--line); }
  .settings-row strong, .settings-row small { display: block; }
  .settings-row strong { font-size: 10px; font-weight: 650; }
  .settings-row small { max-width: 300px; margin-top: 3px; color: var(--muted); font-size: 8px; line-height: 1.45; }
  .settings-row input { width: 54px; padding: 6px; border: 1px solid var(--line); border-radius: 7px; background: var(--surface); color: var(--ink); font-size: 10px; text-align: center; }
  .settings-value { flex: 0 0 auto; padding: 4px 7px; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); font-size: 8px; }
  .settings-ok { color: var(--accent-strong); }
  .settings-actions { display: flex; justify-content: space-between; gap: 8px; padding-top: 16px; border-top: 1px solid var(--line); }
  @media (max-width: 760px) { .topbar { grid-template-columns: 1fr auto; padding: 0 14px; } .primary-nav { position: fixed; right: 14px; bottom: 14px; left: 14px; z-index: 40; justify-content: center; box-shadow: var(--shadow); } .primary-nav button { flex: 1; justify-content: center; } .team-stack { display: none; } }
</style>
