<script lang="ts">
  import { onMount } from "svelte";
  import BacklogSection from "./BacklogSection.svelte";
  import PlanTab from "./PlanTab.svelte";

  /** The backlog column's width, remembered per viewer. */
  const WIDTH_KEY = "mnemo2.plans.backlogWidth";
  const DEFAULT_WIDTH = 380;
  const MIN_WIDTH = 280;
  /** One keyboard resize step, matching the workbench's splitter. */
  const KEY_STEP = 24;

  /** The saved backlog width, or the default when storage is empty or unreadable. */
  function readWidth(): number {
    try {
      const saved = Number.parseInt(localStorage.getItem(WIDTH_KEY) ?? "", 10);
      return Number.isFinite(saved) ? saved : DEFAULT_WIDTH;
    } catch {
      return DEFAULT_WIDTH;
    }
  }

  /** Remember the width for this viewer; blocked storage just keeps it in memory. */
  function saveWidth(next: number): void {
    try {
      localStorage.setItem(WIDTH_KEY, String(next));
    } catch {
      // Private mode or a full quota: the column still resizes for this session.
    }
  }

  /** The page the splitter divides — its width is the clamp's reference. */
  let body = $state<HTMLElement | null>(null);
  let width = $state(readWidth());
  let resizing = $state(false);

  /**
   * Keep the backlog column between 280px and 60% of the page, so the plans
   * column keeps a readable share and the splitter never leaves the view.
   */
  function clampWidth(next: number): number {
    const total = body?.clientWidth || window.innerWidth;
    const max = Math.max(MIN_WIDTH, 0.6 * total);
    return Math.round(Math.min(Math.max(MIN_WIDTH, next), max));
  }

  /** Re-clamp after a narrower window; the saved width returns when it grows. */
  function reclamp(): void {
    width = clampWidth(width);
  }

  /** Re-clamp and remember the width after a drag or a keyboard step. */
  function commitWidth(): void {
    width = clampWidth(width);
    saveWidth(width);
  }

  function startResize(event: PointerEvent) {
    event.preventDefault();
    (event.currentTarget as HTMLElement | null)?.setPointerCapture?.(event.pointerId);
    resizing = true;
  }

  function handleMove(event: PointerEvent): void {
    if (!resizing) return;
    const rect = body?.getBoundingClientRect();
    if (!rect) return;
    width = clampWidth(rect.right - event.clientX);
  }

  function stopResize(): void {
    if (!resizing) return;
    resizing = false;
    commitWidth();
  }

  /** ArrowLeft widens the backlog (the splitter moves left), ArrowRight narrows it. */
  function resizeKeydown(event: KeyboardEvent): void {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    width = clampWidth(width + (event.key === "ArrowLeft" ? KEY_STEP : -KEY_STEP));
    saveWidth(width);
  }

  // No text selection anywhere while the splitter is being dragged.
  $effect(() => {
    if (!resizing) return;
    document.body.classList.add("resizing");
    return () => document.body.classList.remove("resizing");
  });

  // A saved wide column yields to the plans when the window cannot hold both.
  onMount(reclamp);
</script>

<svelte:window
  onpointermove={handleMove}
  onpointerup={stopResize}
  onpointercancel={stopResize}
  onresize={reclamp}
/>

<section class="plans-view" aria-label="Plans">
  <header class="plans-header">
    <h1>Plans</h1>
    <p>Every board plan, open and closed, with the backlog of ideas beside them.</p>
  </header>
  <div class="plans-body" bind:this={body}>
    <div class="plans-column">
      <PlanTab />
    </div>
    <button
      class="splitter"
      class:resizing
      type="button"
      aria-label="Resize backlog"
      title="Drag or use the arrow keys to resize"
      onpointerdown={startResize}
      onkeydown={resizeKeydown}
    ></button>
    <aside class="backlog-column" style={`width: ${width}px`}>
      <BacklogSection />
    </aside>
  </div>
</section>

<style>
  .plans-view { display: flex; flex: 1; min-width: 0; min-height: 0; flex-direction: column; background: var(--canvas); }
  .plans-header { padding: 18px 24px 12px; }
  .plans-header h1 { margin: 0; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 500; letter-spacing: -0.03em; }
  .plans-header p { margin: 6px 0 0; color: var(--muted); font-size: var(--text-body); }
  .plans-body { display: flex; flex: 1; min-height: 0; border-top: 1px solid var(--line); }
  .plans-column { flex: 1; min-width: 0; overflow-y: auto; background: var(--surface-soft); }
  .plans-column :global(.plan-tab) { padding: 16px 20px 32px; }
  .splitter { flex: 0 0 6px; padding: 0; border: 0; background: var(--line); cursor: col-resize; touch-action: none; }
  .splitter:hover, .splitter.resizing, .splitter:focus-visible { background: var(--accent-line); }
  .splitter:focus-visible { outline: 2px solid var(--accent-strong); outline-offset: -2px; }
  .backlog-column { flex: 0 0 auto; min-width: 0; overflow-y: auto; background: var(--surface); }
  .backlog-column :global(.backlog) { padding: 16px 18px 24px; }
  :global(body.resizing) { user-select: none; }
  @media (max-width: 760px) {
    .plans-body { flex-direction: column-reverse; overflow-y: auto; }
    .plans-column, .backlog-column { width: auto !important; overflow: visible; }
    .splitter { display: none; }
  }
</style>
