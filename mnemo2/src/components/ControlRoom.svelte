<script lang="ts">
  import { ListFilter, Square, Trash2, UsersRound } from "@lucide/svelte";
  import { onMount } from "svelte";
  import {
    actions,
    app,
    BOARD_DIVIDER_WIDTH,
    MIN_BOARD_OUTPUT_WIDTH,
    MIN_BOARD_WIDTH,
    defaultBoardWidth,
    isOldWorker,
    selectedWorker,
  } from "../lib/store";
  import type { Worker, WorkerStatus } from "../lib/types";
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
  const oldCount = $derived($app.workers.filter(isOldWorker).length);

  /**
   * The worker list's column: the saved width in pixels, or today's proportion
   * until the user drags the divider (or a project restores a saved width).
   */
  const boardColumns = $derived(
    $app.boardWidth === null
      ? `minmax(${MIN_BOARD_WIDTH}px, 0.88fr) ${BOARD_DIVIDER_WIDTH}px minmax(${MIN_BOARD_OUTPUT_WIDTH}px, 1.12fr)`
      : `${$app.boardWidth}px ${BOARD_DIVIDER_WIDTH}px minmax(${MIN_BOARD_OUTPUT_WIDTH}px, 1fr)`,
  );

  /** The list's real on-screen width: the base a drag or an arrow key starts from. */
  function listWidth(): number {
    // A saved width is also the column the grid lays out, so it is the honest
    // base; the measured column only matters for the default proportion.
    if ($app.boardWidth !== null) return $app.boardWidth;
    return boardEl?.clientWidth || defaultBoardWidth(window.innerWidth);
  }

  let boardEl = $state<HTMLElement | null>(null);
  let dragging = $state(false);
  let dragStartX = 0;
  let dragStartWidth = 0;

  function startDrag(event: PointerEvent) {
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture?.(event.pointerId);
    dragStartX = event.clientX;
    dragStartWidth = listWidth();
    dragging = true;
  }

  function dragMove(event: PointerEvent) {
    if (!dragging) return;
    actions.setBoardWidth(dragStartWidth + (event.clientX - dragStartX));
  }

  function stopDrag() {
    dragging = false;
  }

  function resizeKeydown(event: KeyboardEvent) {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    actions.setBoardWidth(listWidth() + (event.key === "ArrowRight" ? 24 : -24));
  }

  // A saved wide list yields to the worker output when the window cannot hold both.
  onMount(() => actions.reclampBoardWidth());

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

<svelte:window
  onpointermove={dragMove}
  onpointerup={stopDrag}
  onpointercancel={stopDrag}
  onresize={() => actions.reclampBoardWidth()}
/>

<section class="control-room" aria-label="Mission Control">
  <header class="control-header">
    <h1>Mission Control</h1>
    <div class="team-settings">
      <span class="slot-pill">{$app.teamActive} live / {$app.teamSlots} slots</span>
      <button class="button button-quiet" disabled={$app.teamActive === 0} onclick={() => actions.stopAll()}><Square size={11} /> Stop all</button>
      <button class="button button-quiet" title="Delete merged, cancelled, stopped, interrupted, and failed workers, plus finished audits, with their history" disabled={oldCount === 0 || $app.workerDeletion !== null} onclick={() => actions.requestDeleteOldWorkers()}><Trash2 size={11} /> Delete old ({oldCount})</button>
    </div>
  </header>

  <div class="control-layout" style={`--board-columns: ${boardColumns}`}>
    <aside class="worker-board" bind:this={boardEl} aria-label="Worker board">
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
    <button
      class="pane-divider"
      class:resizing={dragging}
      type="button"
      aria-label="Resize worker list"
      title="Drag, or use the arrow keys, to resize the worker list; double-click to reset"
      onpointerdown={startDrag}
      onkeydown={resizeKeydown}
      ondblclick={() => actions.setBoardWidth(null)}
    ></button>
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
  .control-header h1 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 500; letter-spacing: -0.03em; }
  .team-settings { display: flex; align-items: center; gap: 18px; }
  .slot-pill { padding: 5px 9px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink-soft); font-size: var(--text-meta); font-weight: 600; }
  .control-layout { display: grid; flex: 1; min-height: 0; grid-template-columns: var(--board-columns, minmax(300px, 0.88fr) 9px minmax(420px, 1.12fr)); border-top: 1px solid var(--line); }
  .worker-board { min-height: 0; min-width: 0; overflow-y: auto; background: var(--surface); }
  .pane-divider { position: relative; min-width: 0; padding: 0; border: 0; border-radius: 0; background: transparent; cursor: col-resize; touch-action: none; }
  .pane-divider::after { position: absolute; top: 0; bottom: 0; left: 3px; width: 2px; background: var(--line); content: ""; transition: background 120ms ease; }
  .pane-divider:hover::after, .pane-divider.resizing::after, .pane-divider:focus-visible::after { background: var(--accent); }
  .pane-divider:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .board-top { display: flex; justify-content: space-between; padding: 14px 18px 8px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.08em; }
  .board-top > span:first-child { display: inline-flex; align-items: center; gap: 5px; }
  .board-filters { display: flex; gap: 5px; padding: 6px 14px 12px; border-bottom: 1px solid var(--line); }
  .board-filters button { display: inline-flex; align-items: center; gap: 4px; padding: 5px 8px; border: 1px solid transparent; border-radius: 7px; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-meta); }
  .board-filters button.active { border-color: var(--line); background: var(--surface-soft); color: var(--ink); }
  .board-filters button span { display: grid; min-width: 13px; height: 13px; place-items: center; border-radius: 7px; background: var(--surface-soft); font-size: var(--text-meta); }
  .board-filters button.active span { background: var(--surface); }
  .worker-list { padding: 4px 8px 18px; }
  .worker-list section { margin-top: 10px; }
  .worker-list h2 { display: flex; justify-content: space-between; margin: 0 10px 4px; color: var(--muted); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0.08em; text-transform: uppercase; }
  .worker-list h2 span { font-weight: 500; }
  .worker-pane { min-width: 0; min-height: 0; background: var(--canvas); }
  .empty-board, .empty-detail { display: grid; place-items: center; padding: 50px 20px; color: var(--muted); text-align: center; }
  .empty-detail { height: 100%; align-content: center; }
  .empty-detail h2 { margin: 12px 0 0; color: var(--ink-soft); font-family: var(--font-prose); font-size: var(--text-page); font-weight: 500; }
  @media (max-width: 760px) {
    .control-header { padding: 16px; }
    .control-header h1 { font-size: var(--text-page); }
    .control-layout { grid-template-columns: 1fr; grid-template-rows: minmax(220px, 42%) minmax(0, 58%); }
    .worker-board { border-right: 0; border-bottom: 1px solid var(--line); }
    /* The list and the output stack, so there is nothing left to drag. */
    .pane-divider { display: none; }
    .board-filters { overflow-x: auto; }
    .board-filters button { white-space: nowrap; }
  }
</style>
