<script lang="ts">
  // Copyright (c) 2026 Carsten Hess
  // SPDX-License-Identifier: MIT
  // See LICENSE in the repository root.

  /**
   * The Insights panel's steering section: how often each steering note in a
   * tool result fired this app run and how often the agent's next call did what
   * the note asked, plus how the run's file edits split between the file tools
   * and the shell. The counters live on the engine's one loop factory, so the
   * numbers cover the lead and every worker alike.
   */
  import {
    followRate,
    hasSteeringData,
    markerLabel,
    steeringRows,
    type SteeringStats,
  } from "../lib/steering";

  let { stats }: { stats: SteeringStats | null } = $props();

  const rows = $derived(stats === null ? [] : steeringRows(stats));
  /** The edit-freshness note is the only kind that can be blocked. */
  const blocked = $derived(rows.some((row) => row.gated > 0));
</script>

<section class="steering" aria-label="Steering notes">
  <h3>Steering notes</h3>
  <p class="scope">This app run — the lead and every worker.</p>

  {#if !hasSteeringData(stats)}
    <p class="quiet">No steering notes yet this run.</p>
  {:else if stats !== null}
    <p class="followed">
      Followed {stats.switched} of {stats.fired} ({followRate(stats.fired, stats.switched)})
    </p>
    {#if rows.length > 0}
      <div class="table-wrap">
        <table aria-label="Steering notes by kind">
          <thead>
            <tr>
              <th scope="col">Note</th>
              <th scope="col">Shown</th>
              <th scope="col">Followed</th>
              <th scope="col">Rate</th>
              {#if blocked}
                <th scope="col">Blocked</th>
              {/if}
            </tr>
          </thead>
          <tbody>
            {#each rows as row (row.marker)}
              <tr>
                <th scope="row">{markerLabel(row.marker)}</th>
                <td>{row.fired}</td>
                <td>{row.switched}</td>
                <td>{followRate(row.fired, row.switched)}</td>
                {#if blocked}
                  <td>{row.gated}</td>
                {/if}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
    <p
      class="edits"
      title="Shell edits are counted by matching commands such as Set-Content, Out-File, >>, sed -i and tee, so this count can be too high."
    >
      File edits: {stats.file_tool_mutations} with file tools · {stats.shell_mutations} through the
      shell
    </p>
  {/if}
</section>

<style>
  .steering {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 10px;
    padding: 12px 14px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--surface);
  }

  h3 {
    margin: 0;
    font-size: var(--text-title);
    font-weight: 620;
  }

  .scope,
  .quiet,
  .followed,
  .edits {
    margin: 0;
    font-size: var(--text-meta);
  }
  .scope,
  .quiet {
    color: var(--muted);
  }
  .followed,
  .edits {
    color: var(--ink-soft);
  }
  .edits {
    padding-top: 4px;
    border-top: 1px solid var(--line);
  }

  .table-wrap {
    max-height: 220px;
    overflow: auto;
  }
  table {
    border-collapse: collapse;
    width: 100%;
    font-size: var(--text-meta);
    font-variant-numeric: tabular-nums;
  }
  th,
  td {
    padding: 4px 8px;
    border-bottom: 1px solid var(--line);
    text-align: right;
    white-space: nowrap;
  }
  th:first-child,
  td:first-child {
    text-align: left;
  }
  thead th {
    color: var(--muted);
    font-weight: 600;
  }
  tbody th {
    font-weight: 500;
  }
</style>
