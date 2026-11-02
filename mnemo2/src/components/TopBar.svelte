<script lang="ts">
  import { Brain, ChartColumn, ChevronDown, ListChecks, MessageSquare, Network, PanelsTopLeft, Settings, UsersRound, Wallet } from "@lucide/svelte";
  import { actions, app, currentProject, needsYouCount } from "../lib/store";
  import { formatTokenCount, formatUsageCost, usageIsPartial, usageTotalTokens } from "../lib/usage";
  import AvatarStack from "./AvatarStack.svelte";
  import ContextRing from "./ContextRing.svelte";
  import ExternalLeadBadge from "./ExternalLeadBadge.svelte";
  import Sheet from "./Sheet.svelte";
  import SettingsDialog from "./SettingsDialog.svelte";

  const visibleWorkers = $derived($app.workers.filter((worker) => ["working", "in_review", "fixing"].includes(worker.status)));
  const activeCount = $derived($app.workers.filter((worker) => ["working", "in_review", "fixing", "merging"].includes(worker.status)).length);
  const usage = $derived($app.projectUsage);
  const costLabel = $derived(formatUsageCost(usage.costMicros));
  const usagePartial = $derived(usageIsPartial(usage));
  const totalTokens = $derived(usageTotalTokens(usage));
  const usageLabel = $derived(
    `Project cost ${costLabel}${usagePartial ? ", partial estimate: some usage has no price" : ""}. Open the Cost tab.`,
  );
  let usageTipOpen = $state(false);
  let appSettingsOpen = $state(false);

  function handleTeam() {
    // Team always opens Mission Control directly — never the right-side peek.
    actions.openTeam();
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
    <button class:active={$app.route === "team"} aria-pressed={$app.route === "team"} onclick={handleTeam}><UsersRound size={14} /> Team
      {#if $needsYouCount > 0}<span class="nav-badge">{$needsYouCount}</span>{/if}
    </button>
    <button class:active={$app.route === "cost"} aria-pressed={$app.route === "cost"} onclick={actions.openCost}><ChartColumn size={14} /> Cost</button>
    <button class:active={$app.route === "graph"} aria-pressed={$app.route === "graph"} onclick={actions.openGraph}><Network size={14} /> Graph</button>
    <button class:active={$app.route === "plans"} aria-pressed={$app.route === "plans"} onclick={actions.openPlans}><ListChecks size={14} /> Plans</button>
    <button class:active={$app.route === "memory"} aria-pressed={$app.route === "memory"} onclick={actions.openMemory}><Brain size={14} /> Memory</button>
  </nav>

  <div class="topbar-right">
    <button class="team-stack" onclick={handleTeam} title="Open Mission Control">
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
    <button class="icon-button" aria-label="Settings" title="Project and install settings" onclick={() => (appSettingsOpen = true)}><Settings size={15} /></button>
    <ExternalLeadBadge />
    <ContextRing />
    <div class="usage-chip">
      <button
        class="budget-chip"
        aria-label={usageLabel}
        aria-describedby="project-usage-detail"
        onmouseenter={() => (usageTipOpen = true)}
        onmouseleave={() => (usageTipOpen = false)}
        onfocus={() => (usageTipOpen = true)}
        onblur={() => (usageTipOpen = false)}
        onclick={actions.openCost}
      >
        <Wallet size={13} />
        <span class="usage-cost">{costLabel}{#if usagePartial}<span class="usage-partial" aria-hidden="true">+</span>{/if}</span>
      </button>
      <div id="project-usage-detail" class="usage-tip" class:open={usageTipOpen} role="tooltip">
        <strong>Project usage</strong>
        <dl>
          <div><dt>Cached input</dt><dd>{formatTokenCount(usage.cachedTokens)} tokens</dd></div>
          <div><dt>Uncached input</dt><dd>{formatTokenCount(usage.inputTokens)} tokens</dd></div>
          <div><dt>Output</dt><dd>{formatTokenCount(usage.outputTokens)} tokens</dd></div>
          <div><dt>Total</dt><dd>{formatTokenCount(totalTokens)} tokens</dd></div>
        </dl>
        {#if usagePartial}
          <p class="usage-note">No price for: {usage.unpricedModels.join(", ")}. The cost covers priced usage only.</p>
        {:else if totalTokens === 0}
          <p class="usage-note">No usage recorded for this project yet.</p>
        {/if}
      </div>
    </div>
  </div>
</header>

<SettingsDialog open={appSettingsOpen} initialPage="project" onclose={() => (appSettingsOpen = false)} />

<Sheet open={$app.closeProjectPending} title="Close project and stop the team?" eyebrow="ACTIVE SESSIONS" onclose={actions.cancelCloseProject}>
  <p class="settings-copy">The lead and any live workers will stop. Branches, worktrees, and transcripts remain available for review.</p>
  <div class="settings-actions">
    <button class="button button-quiet" onclick={actions.cancelCloseProject}>Keep working</button>
    <button class="button button-primary" onclick={actions.confirmCloseProject}>Stop and close</button>
  </div>
</Sheet>

<style>
  .topbar { position: relative; z-index: 20; display: grid; grid-template-columns: 1fr auto 1fr; align-items: center; min-height: 64px; padding: 0 22px; border-bottom: 1px solid var(--line); background: var(--overlay); backdrop-filter: blur(18px); }
  .topbar-left, .topbar-right { display: flex; align-items: center; gap: 14px; }
  .topbar-right { justify-content: flex-end; }
  .brand-mark { display: grid; width: 30px; height: 30px; place-items: center; border: 1px solid var(--line-strong); border-radius: 9px; background: var(--surface); color: var(--accent-strong); font-family: var(--font-prose); font-size: var(--text-title); }
  .project-switch { display: grid; grid-template-columns: 1fr auto; column-gap: 8px; padding: 0; border: 0; background: transparent; color: var(--ink); cursor: pointer; text-align: left; }
  .project-switch strong { font-size: var(--text-body); font-weight: 650; }
  .project-switch span { grid-row: 2; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); }
  .project-switch :global(svg) { grid-row: 1 / 3; grid-column: 2; align-self: center; color: var(--muted); }
  .primary-nav { display: flex; gap: 3px; padding: 3px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface-soft); }
  .primary-nav button { display: inline-flex; align-items: center; gap: 6px; padding: 6px 12px; border: 0; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-body); font-weight: 600; }
  .primary-nav button.active { background: var(--surface); color: var(--ink); box-shadow: var(--shadow-nav); }
  .nav-badge { display: grid; min-width: 15px; height: 15px; place-items: center; border-radius: 8px; background: var(--warning-soft); color: var(--warning); font-size: var(--text-meta); }
  .team-stack { display: flex; align-items: center; gap: 8px; padding: 0; border: 0; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-meta); }
  .budget-chip { display: inline-flex; align-items: center; gap: 5px; padding: 6px 9px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink-soft); cursor: pointer; font-size: var(--text-meta); }
  .budget-chip:hover { border-color: var(--line-strong); }
  .usage-chip { position: relative; display: inline-flex; }
  .usage-cost { font-variant-numeric: tabular-nums; }
  .usage-partial { margin-left: -1px; color: var(--warning); font-weight: 700; }
  .usage-tip { position: absolute; top: calc(100% + 7px); right: 0; z-index: 30; width: 228px; padding: 10px 12px; border: 1px solid var(--line-strong); border-radius: 10px; background: var(--surface); box-shadow: var(--shadow); opacity: 0; visibility: hidden; pointer-events: none; transition: opacity 120ms ease; }
  .usage-tip.open { opacity: 1; visibility: visible; }
  .usage-tip strong { display: block; margin-bottom: 7px; font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.04em; text-transform: uppercase; color: var(--muted); }
  .usage-tip dl { display: grid; gap: 4px; margin: 0; }
  .usage-tip dl div { display: flex; justify-content: space-between; gap: 10px; }
  .usage-tip dt { color: var(--muted); font-size: var(--text-meta); }
  .usage-tip dd { margin: 0; color: var(--ink); font-family: var(--font-mono); font-size: var(--text-meta); font-variant-numeric: tabular-nums; }
  .usage-note { margin: 8px 0 0; color: var(--warning); font-size: var(--text-meta); line-height: 1.45; }
  .settings-copy { margin: 0 0 16px; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }
  .settings-actions { display: flex; justify-content: space-between; gap: 8px; padding-top: 16px; border-top: 1px solid var(--line); }
  @media (max-width: 760px) { .topbar { grid-template-columns: 1fr auto; padding: 0 14px; } .primary-nav { position: fixed; right: 14px; bottom: 14px; left: 14px; z-index: 40; justify-content: center; box-shadow: var(--shadow); } .primary-nav button { flex: 1; justify-content: center; } .team-stack { display: none; } }
</style>
