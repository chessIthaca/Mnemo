<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { Check } from "@lucide/svelte";
  import { app } from "../lib/store";
  import { runningPasses, type IndexPass, type PassKind } from "../lib/indexingEngine";

  type Kind = PassKind;
  /** How long a just-finished pass stays visible, so a fast pass is still seen. */
  const FINISHED_MS = 4000;

  /** What each pass is doing, as its running line reads it. */
  const RUNNING_TEXT: Record<Kind, string> = {
    code: "Indexing code",
    memory: "Updating memory",
    laya: "Checking Laya",
  };
  /** The unit each pass counts. */
  const UNIT_TEXT: Record<Kind, string> = {
    code: "files",
    memory: "sources",
    laya: "tasks",
  };
  /** Each pass's progress bar name. */
  const PROGRESS_LABEL: Record<Kind, string> = {
    code: "Code index progress",
    memory: "Memory update progress",
    laya: "Laya check progress",
  };
  /** The confirmation each pass shows for a moment after it ends. */
  const FINISHED_TEXT: Record<Kind, string> = {
    code: "Code index up to date",
    memory: "Memory up to date",
    laya: "Laya checked",
  };
  /** What each pass shows when it fails. */
  const FAILED_TEXT: Record<Kind, string> = {
    code: "Code index failed",
    memory: "Memory update failed",
    laya: "Laya check failed",
  };

  const passes = $derived(runningPasses($app.indexing));
  const failed = $derived(
    ([
      ["code", $app.indexing?.code],
      ["memory", $app.indexing?.memory],
      ["laya", $app.indexing?.laya],
    ] as const)
      .filter(([, pass]) => pass?.state === "failed")
      .map(([kind, pass]) => ({ kind, error: pass?.detail ?? "unknown error" })),
  );

  // A pass that finishes while this is mounted shows "up to date" briefly; one
  // that was already done on mount stays hidden.
  let seenRunning: Partial<Record<Kind, boolean>> = {};
  let finished = $state<{ kind: Kind; title: string }[]>([]);
  const timers = new Map<Kind, ReturnType<typeof setTimeout>>();

  $effect(() => {
    const indexing = $app.indexing;
    untrack(() => track(indexing));
  });

  function track(indexing: typeof $app.indexing) {
    for (const kind of ["code", "memory", "laya"] as const) {
      const pass = indexing?.[kind] ?? null;
      if (pass?.state === "running") {
        seenRunning[kind] = true;
        finished = finished.filter((item) => item.kind !== kind);
      } else if (pass?.state === "done" && seenRunning[kind]) {
        seenRunning[kind] = false;
        finished = [...finished.filter((item) => item.kind !== kind), { kind, title: pass.detail ?? "" }];
        clearTimeout(timers.get(kind));
        timers.set(kind, setTimeout(() => {
          finished = finished.filter((item) => item.kind !== kind);
        }, FINISHED_MS));
      }
    }
  }

  onDestroy(() => {
    for (const timer of timers.values()) clearTimeout(timer);
  });

  function label(kind: Kind, pass: IndexPass): string {
    const what = RUNNING_TEXT[kind];
    if (pass.total === 0) return what;
    return `${what} · ${pass.done.toLocaleString()}/${pass.total.toLocaleString()} ${UNIT_TEXT[kind]}`;
  }

  function fraction(pass: IndexPass): number | null {
    return pass.total > 0 ? Math.min(1, pass.done / pass.total) : null;
  }
</script>

{#each passes as { kind, pass } (kind)}
  {@const share = fraction(pass)}
  <span class="indexing" role="status">
    <span>{label(kind, pass)}</span>
    <span
      class="bar"
      class:counting={share === null}
      role="progressbar"
      aria-label={PROGRESS_LABEL[kind]}
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={share === null ? undefined : Math.round(share * 100)}
    ><span style:width={share === null ? undefined : `${share * 100}%`}></span></span>
  </span>
{/each}
{#each finished as { kind, title } (kind)}
  <span class="indexing finished" role="status" title={title || undefined}>
    <Check size={11} aria-hidden="true" />
    {FINISHED_TEXT[kind]}
  </span>
{/each}
{#each failed as { kind, error } (kind)}
  <span class="indexing failed" title={error}>
    {FAILED_TEXT[kind]}
  </span>
{/each}

<style>
  .indexing { display: inline-flex; align-items: center; gap: 7px; font-size: var(--text-body); font-weight: 500; letter-spacing: 0; white-space: nowrap; font-variant-numeric: tabular-nums; }
  .failed { color: var(--danger); cursor: help; }
  .finished { gap: 4px; color: var(--accent-strong); animation: fade-out 600ms ease 3400ms forwards; }
  @keyframes fade-out { to { opacity: 0; } }
  .bar { position: relative; width: 64px; height: 3px; overflow: hidden; border-radius: 2px; background: var(--line); }
  .bar > span { position: absolute; inset: 0 auto 0 0; border-radius: inherit; background: var(--accent); transition: width 240ms ease; }
  .bar.counting > span { width: 30%; animation: sweep 1.3s ease-in-out infinite; }
  @keyframes sweep { from { left: -30%; } to { left: 100%; } }
  @media (prefers-reduced-motion: reduce) { .bar.counting > span { animation: none; left: 35%; } }
</style>
