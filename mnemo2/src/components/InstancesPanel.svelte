<script lang="ts">
  import { PanelsTopLeft } from "@lucide/svelte";
  import { onMount } from "svelte";
  import {
    INSTANCES_REFRESH_MS,
    instances as liveTransport,
    type InstanceRow,
    type InstancesTransport,
  } from "../lib/instances";
  import { actions } from "../lib/store";
  import { formatUsageCost } from "../lib/usage";

  /**
   * Every Mnemo2 window on this machine (plan d3336a8a): the open project, the
   * running and waiting workers, and today's cost, refreshed every 10 s while
   * Mission Control is open. A window that does not answer keeps its row and
   * says so; clicking one brings its window to the front.
   */
  let {
    transport = liveTransport,
  }: {
    /** The backend transport; the component test injects a fake. */
    transport?: InstancesTransport;
  } = $props();

  let rows = $state<InstanceRow[]>([]);

  /** The project folder's name; the start screen has no project. */
  function projectName(row: InstanceRow): string {
    const parts = (row.project ?? "").split(/[\\/]/).filter(Boolean);
    return parts.length > 0 ? parts[parts.length - 1] : "Start screen";
  }

  async function refresh(): Promise<void> {
    try {
      rows = await transport.list();
    } catch {
      // A convenience view: a failed read keeps the last rows rather than
      // taking Mission Control down with it.
    }
  }

  async function show(row: InstanceRow): Promise<void> {
    try {
      await transport.show(row.pid);
    } catch (error) {
      actions.showToast(String(error), "error");
    }
  }

  onMount(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), INSTANCES_REFRESH_MS);
    return () => clearInterval(timer);
  });
</script>

{#if rows.length > 0}
  <section class="instances" aria-label="Mnemo2 instances on this machine">
    <span class="instances-title"><PanelsTopLeft size={12} /> Instances <span class="instances-count">{rows.length}</span></span>
    <ul>
      {#each rows as row (row.pid)}
        <li class:silent={!row.answered}>
          {#if row.is_self}
            <span class="instance" aria-current="true">{@render content(row)}</span>
          {:else if row.answered}
            <button
              class="instance"
              type="button"
              title={`Show ${row.project ?? "the start screen"} (v${row.version})`}
              onclick={() => void show(row)}
            >{@render content(row)}</button>
          {:else}
            <span class="instance" title={`${row.project ?? "Start screen"} (v${row.version}) did not answer`}>{@render content(row)}</span>
          {/if}
        </li>
      {/each}
    </ul>
  </section>
{/if}

{#snippet content(row: InstanceRow)}
  <span class="instance-project" title={row.project ?? "Start screen"}>{projectName(row)}</span>
  {#if row.is_self}<span class="instance-badge">this window</span>{/if}
  {#if row.answered}
    <span class="instance-counts">
      <span>{row.workers_running} running</span>
      <span>{row.workers_waiting} waiting</span>
    </span>
    <span class="instance-cost">{formatUsageCost(row.cost_today_micros)} today</span>
  {:else}
    <span class="instance-silent">not answering</span>
  {/if}
  <span class="instance-version">v{row.version}</span>
{/snippet}

<style>
  .instances { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 14px; padding: 0 24px 12px; }
  .instances-title { display: inline-flex; align-items: center; gap: 5px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.08em; text-transform: uppercase; }
  .instances-count { display: grid; min-width: 15px; height: 15px; place-items: center; border-radius: 8px; background: var(--surface-soft); font-size: var(--text-meta); letter-spacing: 0; }
  .instances ul { display: flex; flex-wrap: wrap; gap: 6px; margin: 0; padding: 0; list-style: none; }
  .instance { display: inline-flex; align-items: center; gap: 8px; padding: 5px 9px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink-soft); font-size: var(--text-meta); }
  button.instance { cursor: pointer; }
  button.instance:hover { border-color: var(--line-strong); }
  button.instance:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .instance-project { max-width: 220px; overflow: hidden; color: var(--ink); font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
  .instance-badge { padding: 1px 5px; border-radius: 6px; background: var(--accent-soft); color: var(--accent-strong); font-size: var(--text-meta); }
  .instance-counts { display: inline-flex; gap: 6px; color: var(--muted); font-variant-numeric: tabular-nums; }
  .instance-cost { color: var(--ink-soft); font-variant-numeric: tabular-nums; }
  .instance-version { color: var(--muted); font-family: var(--font-mono); }
  .instance-silent { color: var(--warning); font-weight: 600; }
  li.silent .instance { border-style: dashed; }
  @container control-room (max-width: 760px) {
    .instances { padding: 0 16px 10px; }
  }
</style>
