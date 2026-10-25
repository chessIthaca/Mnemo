<script lang="ts">
  import { ListFilter, UsersRound } from "@lucide/svelte";
  import { actions, app, selectedWorker } from "../lib/store";
  import type { Worker, WorkerStatus } from "../lib/types";
  import AvatarStack from "./AvatarStack.svelte";
  import WorkerDetail from "./WorkerDetail.svelte";
  import WorkerRow from "./WorkerRow.svelte";

  type WorkerGroup = { label: string; workers: Worker[] };
  const activeStatuses: WorkerStatus[] = [
    "dispatching",
    "working",
    "idle",
    "paused",
    "interrupted",
    "in_review",
    "fixing",
    "merging",
  ];
  const doneStatuses: WorkerStatus[] = ["completed", "merged", "stopped", "cancelled"];
  const needsCount = $derived($app.workers.filter((worker) => worker.needsYou).length);
  const activeCount = $derived($app.workers.filter((worker) => activeStatuses.includes(worker.status)).length);
  const doneCount = $derived($app.workers.filter((worker) => doneStatuses.includes(worker.status)).length);

  let brief = $state("");
  let dispatching = $state(false);

  async function dispatch(event: SubmitEvent) {
    event.preventDefault();
    if (!brief.trim() || dispatching) return;
    dispatching = true;
    try {
      await actions.dispatchWorker(brief);
      brief = "";
    } finally {
      dispatching = false;
    }
  }

  const groups = $derived.by((): WorkerGroup[] => {
    const filter = $app.teamFilter;
    const filtered = $app.workers.filter((worker) => {
      if (filter === "needs") return worker.needsYou;
      if (filter === "active") return activeStatuses.includes(worker.status);
      if (filter === "done") return doneStatuses.includes(worker.status);
      return true;
    });
    if (filter !== "all") {
      const label = filter === "needs" ? "Needs you" : filter === "active" ? "Active" : "Done";
      return [{ label, workers: filtered }];
    }
    return [
      { label: "Needs you", workers: filtered.filter((worker) => worker.needsYou) },
      {
        label: "Active",
        workers: filtered.filter(
          (worker) => activeStatuses.includes(worker.status) && !worker.needsYou,
        ),
      },
      {
        label: "Done",
        workers: filtered.filter(
          (worker) => doneStatuses.includes(worker.status) && !worker.needsYou,
        ),
      },
    ].filter((group) => group.workers.length > 0);
  });
</script>

<section class="control-room" aria-label="Mission Control">
  <header class="control-header">
    <h1>Mission Control</h1>
    <div class="team-settings">
      <span class="slot-pill">{$app.teamActive} live / {$app.teamSlots} slots</span>
    </div>
  </header>

  <form class="dispatch-bar" onsubmit={dispatch}>
    <input
      bind:value={brief}
      aria-label="New worker brief"
      placeholder="Delegate a task - the worker gets its own branch and worktree"
    />
    <button class="button button-primary" type="submit" disabled={!brief.trim() || dispatching}>
      {dispatching ? "Dispatching..." : "Dispatch"}
    </button>
  </form>

  <div class="control-summary">
    <AvatarStack workers={$app.workers} max={6} />
    <strong>{$app.workers.length} workers</strong>
    <span>·</span><span>{$app.teamActive} live · {$app.teamSlots} slots</span>
  </div>

  <div class="control-layout">
    <aside class="worker-board" aria-label="Worker board">
      <div class="board-top"><span><UsersRound size={13} /> Tasks</span></div>
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
        <div class="empty-detail"><UsersRound size={24} /><h2>Select a task</h2></div>
      {/if}
    </main>
  </div>
</section>

<style>
  .control-room { display: flex; flex: 1; min-width: 0; min-height: 0; flex-direction: column; }
  .control-header { display: flex; align-items: center; justify-content: space-between; gap: 24px; padding: 18px 24px 12px; }
  .control-header h1 { margin: 0; font-family: var(--font-prose); font-size: 28px; font-weight: 500; letter-spacing: -0.03em; }
  .team-settings { display: flex; align-items: center; gap: 18px; }
  .slot-pill { padding: 5px 9px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink-soft); font-size: 9px; font-weight: 600; }
  .dispatch-bar { display: flex; gap: 8px; padding: 0 24px 12px; }
  .dispatch-bar input { flex: 1; min-width: 0; padding: 9px 12px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); color: var(--ink); font-size: 10px; }
  .dispatch-bar button { flex: 0 0 auto; }
  .control-summary { display: flex; align-items: center; gap: 8px; padding: 0 24px 12px; color: var(--muted); font-size: 8px; }
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
  .empty-detail h2 { margin: 12px 0 0; color: var(--ink-soft); font-family: var(--font-prose); font-size: 22px; font-weight: 500; }
  @media (max-width: 980px) {
    .control-layout { grid-template-columns: minmax(290px, 0.8fr) minmax(360px, 1.2fr); }
    .control-summary > span:last-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  }

  @media (max-width: 760px) {
    .control-header { padding: 16px; }
    .control-header h1 { font-size: 26px; }
    .control-summary { flex-wrap: wrap; row-gap: 4px; padding: 0 16px 12px; }
    .control-summary > span:last-child { flex-basis: 100%; overflow: visible; text-overflow: clip; white-space: normal; }
    .control-layout { grid-template-columns: 1fr; grid-template-rows: minmax(220px, 42%) minmax(0, 58%); }
    .worker-board { border-right: 0; border-bottom: 1px solid var(--line); }
    .board-filters { overflow-x: auto; }
    .board-filters button { white-space: nowrap; }
  }
</style>
