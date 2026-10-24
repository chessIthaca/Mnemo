<script lang="ts">
  import { ArrowRight, X } from "@lucide/svelte";
  import { actions, app, currentProject } from "../lib/store";
  import WorkerRow from "./WorkerRow.svelte";

  const needsYou = $derived($app.workers.filter((worker) => worker.needsYou));
  const reviewing = $derived($app.workers.filter((worker) => worker.status === "in_review" && !worker.needsYou));
  const working = $derived($app.workers.filter((worker) => ["working", "fixing", "merging", "dispatching"].includes(worker.status)));
</script>

<div class="peek-layer">
  <button class="peek-backdrop" aria-label="Close team peek" onclick={actions.toggleTeamPeek}></button>
  <aside class="team-peek" aria-label="Team peek">
    <header><div><p>THIS PROJECT</p><h2>Team</h2></div><button class="icon-button" aria-label="Close team peek" onclick={actions.toggleTeamPeek}><X size={15} /></button></header>
    <div class="peek-budget"><span>Budget</span><strong>${$currentProject?.budgetSpent.toFixed(2)} <small>/ ${$currentProject?.budgetLimit}</small></strong><div><span style={`width: ${(($currentProject?.budgetSpent ?? 0) / ($currentProject?.budgetLimit ?? 1)) * 100}%`}></span></div></div>

    <div class="peek-scroll">
      {#if needsYou.length > 0}
        <section><h3>Needs you <span>{needsYou.length}</span></h3>{#each needsYou as worker (worker.id)}<WorkerRow {worker} compact onSelect={actions.selectWorker} />{/each}</section>
      {/if}
      {#if reviewing.length > 0}
        <section><h3>In review <span>{reviewing.length}</span></h3>{#each reviewing as worker (worker.id)}<WorkerRow {worker} compact onSelect={actions.selectWorker} />{/each}</section>
      {/if}
      <section><h3>Working <span>{working.length}</span></h3>{#each working.slice(0, 5) as worker (worker.id)}<WorkerRow {worker} compact onSelect={actions.selectWorker} />{/each}</section>
    </div>
    <footer><span>{$app.workers.length} workers · {$currentProject?.workerSlots} slots</span><button onclick={actions.openTeam}>Control room <ArrowRight size={13} /></button></footer>
  </aside>
</div>

<style>
  .peek-layer { position: fixed; inset: 64px 0 0; z-index: 35; pointer-events: none; }
  .peek-backdrop { position: absolute; inset: 0; width: 100%; border: 0; background: rgb(38 40 34 / 5%); pointer-events: auto; }
  .team-peek { position: absolute; top: 10px; right: 12px; bottom: 12px; display: flex; width: min(380px, calc(100vw - 24px)); flex-direction: column; overflow: hidden; border: 1px solid var(--line); border-radius: 18px; background: var(--surface); box-shadow: var(--shadow); pointer-events: auto; }
  header { display: flex; align-items: center; justify-content: space-between; padding: 18px; }
  header p { margin: 0 0 3px; color: var(--muted); font-size: 7px; letter-spacing: 0.12em; }
  header h2 { margin: 0; font-family: var(--font-prose); font-size: 27px; font-weight: 500; }
  .peek-budget { margin: 0 18px 12px; padding: 12px; border-radius: 10px; background: var(--surface-soft); color: var(--muted); font-size: 9px; }
  .peek-budget > strong { float: right; color: var(--ink-soft); font-weight: 550; }
  .peek-budget small { color: var(--muted); font-weight: 400; }
  .peek-budget > div { height: 3px; margin-top: 9px; overflow: hidden; border-radius: 3px; background: var(--line); }
  .peek-budget > div span { display: block; height: 100%; background: var(--accent); }
  .peek-scroll { flex: 1; overflow-y: auto; padding: 0 10px 10px; }
  section { margin-top: 12px; }
  section h3 { display: flex; justify-content: space-between; margin: 0 8px 5px; color: var(--muted); font-size: 8px; font-weight: 650; letter-spacing: 0.08em; text-transform: uppercase; }
  section h3 span { font-weight: 500; }
  footer { display: flex; align-items: center; justify-content: space-between; padding: 13px 18px; border-top: 1px solid var(--line); color: var(--muted); font-size: 9px; }
  footer button { display: inline-flex; align-items: center; gap: 5px; padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: 10px; font-weight: 650; }
  @media (max-width: 720px) { .peek-layer { inset: 64px 0 0; } .team-peek { inset: 0; width: 100%; border-radius: 0; } footer { margin-bottom: 58px; } }
</style>
