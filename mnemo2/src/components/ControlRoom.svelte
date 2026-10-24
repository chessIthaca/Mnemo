<script lang="ts">
  import { ListFilter, UsersRound } from "@lucide/svelte";
  import { actions, app, currentProject, selectedWorker } from "../lib/store";
  import type { Worker, WorkerStatus } from "../lib/types";
  import AvatarStack from "./AvatarStack.svelte";
  import WorkerDetail from "./WorkerDetail.svelte";
  import WorkerRow from "./WorkerRow.svelte";

  type WorkerGroup = { label: string; workers: Worker[] };
  const activeStatuses: WorkerStatus[] = ["dispatching", "working", "in_review", "fixing", "merging"];
  const needsCount = $derived($app.workers.filter((worker) => worker.needsYou).length);
  const activeCount = $derived($app.workers.filter((worker) => activeStatuses.includes(worker.status)).length);
  const doneCount = $derived($app.workers.filter((worker) => ["merged", "cancelled"].includes(worker.status)).length);

  const groups = $derived.by((): WorkerGroup[] => {
    const filter = $app.teamFilter;
    const filtered = $app.workers.filter((worker) => {
      if (filter === "needs") return worker.needsYou;
      if (filter === "active") return activeStatuses.includes(worker.status);
      if (filter === "done") return ["merged", "cancelled"].includes(worker.status);
      return true;
    });
    if (filter !== "all") {
      const label = filter === "needs" ? "Needs you" : filter === "active" ? "Active" : "Done";
      return [{ label, workers: filtered }];
    }
    return [
      { label: "Needs you", workers: filtered.filter((worker) => worker.needsYou) },
      { label: "Working", workers: filtered.filter((worker) => ["dispatching", "working", "fixing", "merging"].includes(worker.status) && !worker.needsYou) },
      { label: "In review", workers: filtered.filter((worker) => worker.status === "in_review" && !worker.needsYou) },
      { label: "Done", workers: filtered.filter((worker) => ["merged", "cancelled"].includes(worker.status) && !worker.needsYou) },
    ].filter((group) => group.workers.length > 0);
  });

  function updateSlots(event: Event) {
    actions.setWorkerSlots((event.currentTarget as HTMLInputElement).valueAsNumber);
  }
</script>

<section class="control-room" aria-label="Mission Control">
  <header class="control-header">
    <div><p>PROJECT / TEAM</p><h1>Mission Control</h1><span>Exceptions, review, and merges — not an approval inbox.</span></div>
    <div class="team-settings">
      <label><span>Worker slots</span><input aria-label="Worker slots" type="number" min="1" step="1" value={$currentProject?.workerSlots ?? 1} onchange={updateSlots} /><small>per project</small></label>
      <div class="room-budget"><span>Project budget <strong>${$currentProject?.budgetSpent.toFixed(2)} / ${$currentProject?.budgetLimit}</strong></span><div><span style={`width: ${(($currentProject?.budgetSpent ?? 0) / ($currentProject?.budgetLimit ?? 1)) * 100}%`}></span></div></div>
    </div>
  </header>

  <div class="control-summary">
    <AvatarStack workers={$app.workers} max={6} />
    <strong>{$app.workers.length} workers</strong>
    <span>·</span><span>{$currentProject?.workerSlots} slots</span><span>·</span><span>Merges serialized into {$currentProject?.branch}</span>
  </div>

  <div class="control-layout">
    <aside class="worker-board" aria-label="Worker board">
      <div class="board-top"><span><UsersRound size={13} /> TEAM BOARD</span><span>THE LEAD IS IN CHAT</span></div>
      <nav class="board-filters" aria-label="Filter workers">
        <button class:active={$app.teamFilter === "all"} aria-pressed={$app.teamFilter === "all"} onclick={() => actions.setTeamFilter("all")}><ListFilter size={11} /> All <span>{$app.workers.length}</span></button>
        <button class:active={$app.teamFilter === "needs"} aria-pressed={$app.teamFilter === "needs"} onclick={() => actions.setTeamFilter("needs")}>Needs you <span>{needsCount}</span></button>
        <button class:active={$app.teamFilter === "active"} aria-pressed={$app.teamFilter === "active"} onclick={() => actions.setTeamFilter("active")}>Active <span>{activeCount}</span></button>
        <button class:active={$app.teamFilter === "done"} aria-pressed={$app.teamFilter === "done"} onclick={() => actions.setTeamFilter("done")}>Done <span>{doneCount}</span></button>
      </nav>
      <div class="worker-list">
        {#each groups as group (group.label)}
          <section><h2>{group.label}<span>{group.workers.length}</span></h2>{#each group.workers as worker (worker.id)}<WorkerRow {worker} selected={$app.selectedWorkerId === worker.id} onSelect={actions.selectWorker} />{/each}</section>
        {:else}
          <div class="empty-board">No workers in this view.</div>
        {/each}
      </div>
    </aside>
    <main class="worker-pane">
      {#if $selectedWorker}
        <WorkerDetail worker={$selectedWorker} />
      {:else}
        <div class="empty-detail"><UsersRound size={24} /><h2>Select a worker</h2><p>Review the brief, checks, findings, and conversation without opening a tool log.</p></div>
      {/if}
    </main>
  </div>
</section>

<style>
  .control-room { display: flex; flex: 1; min-width: 0; min-height: 0; flex-direction: column; }
  .control-header { display: flex; align-items: center; justify-content: space-between; gap: 24px; padding: 22px 28px 16px; }
  .control-header p { margin: 0 0 4px; color: var(--muted); font-size: 8px; letter-spacing: 0.12em; }
  .control-header h1 { margin: 0; font-family: var(--font-prose); font-size: 31px; font-weight: 500; letter-spacing: -0.03em; }
  .control-header > div:first-child > span { display: block; margin-top: 5px; color: var(--muted); font-size: 10px; }
  .team-settings { display: flex; align-items: center; gap: 18px; }
  .team-settings label { display: grid; grid-template-columns: auto 58px; align-items: center; gap: 3px 8px; color: var(--ink-soft); font-size: 9px; }
  .team-settings label small { grid-column: 1 / 3; color: var(--muted); font-size: 7px; text-align: right; }
  .team-settings input { width: 58px; padding: 6px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink); font-size: 10px; text-align: center; }
  .room-budget { width: 160px; color: var(--muted); font-size: 8px; }
  .room-budget > span { display: flex; justify-content: space-between; }
  .room-budget strong { color: var(--ink-soft); font-weight: 550; }
  .room-budget > div { height: 3px; margin-top: 7px; overflow: hidden; border-radius: 3px; background: var(--line); }
  .room-budget > div span { display: block; height: 100%; background: var(--accent); }
  .control-summary { display: flex; align-items: center; gap: 8px; padding: 0 28px 16px; color: var(--muted); font-size: 8px; }
  .control-summary strong { color: var(--ink-soft); font-size: 9px; font-weight: 600; }
  .control-layout { display: grid; flex: 1; min-height: 0; grid-template-columns: minmax(340px, 0.88fr) minmax(430px, 1.12fr); border-top: 1px solid var(--line); }
  .worker-board { min-height: 0; overflow-y: auto; border-right: 1px solid var(--line); background: var(--surface); }
  .board-top { display: flex; justify-content: space-between; padding: 14px 18px 8px; color: var(--muted); font-size: 7px; letter-spacing: 0.08em; }
  .board-top > span:first-child { display: inline-flex; align-items: center; gap: 5px; }
  .board-filters { display: flex; gap: 5px; padding: 6px 14px 12px; border-bottom: 1px solid var(--line); }
  .board-filters button { display: inline-flex; align-items: center; gap: 4px; padding: 5px 8px; border: 1px solid transparent; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; font-size: 8px; }
  .board-filters button.active { border-color: var(--line); background: var(--surface-soft); color: var(--ink); }
  .board-filters button span { display: grid; min-width: 13px; height: 13px; place-items: center; border-radius: 7px; background: var(--surface-soft); font-size: 7px; }
  .board-filters button.active span { background: var(--surface); }
  .worker-list { padding: 4px 8px 18px; }
  .worker-list section { margin-top: 10px; }
  .worker-list h2 { display: flex; justify-content: space-between; margin: 0 10px 4px; color: var(--muted); font-size: 8px; font-weight: 650; letter-spacing: 0.08em; text-transform: uppercase; }
  .worker-list h2 span { font-weight: 500; }
  .worker-pane { min-width: 0; min-height: 0; background: var(--canvas); }
  .empty-board, .empty-detail { display: grid; place-items: center; padding: 50px 20px; color: var(--muted); text-align: center; }
  .empty-detail { height: 100%; align-content: center; }
  .empty-detail h2 { margin: 12px 0 4px; color: var(--ink-soft); font-family: var(--font-prose); font-size: 22px; font-weight: 500; }
  .empty-detail p { max-width: 300px; font-size: 10px; line-height: 1.5; }
  @media (max-width: 980px) {
    .control-header { align-items: flex-start; }
    .room-budget { display: none; }
    .control-layout { grid-template-columns: minmax(290px, 0.8fr) minmax(360px, 1.2fr); }
    .control-summary > span:last-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  }
  @media (max-width: 760px) {
    .control-header { padding: 16px; }
    .control-header > div:first-child > span { display: none; }
    .control-header h1 { font-size: 26px; }
    .control-summary { flex-wrap: wrap; row-gap: 4px; padding: 0 16px 12px; }
    .control-summary > span:last-child { flex-basis: 100%; overflow: visible; text-overflow: clip; white-space: normal; }
    .control-layout { grid-template-columns: 1fr; grid-template-rows: minmax(220px, 42%) minmax(0, 58%); }
    .worker-board { border-right: 0; border-bottom: 1px solid var(--line); }
    .board-filters { overflow-x: auto; }
    .board-filters button { white-space: nowrap; }
  }
</style>
