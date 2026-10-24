<script lang="ts">
  import { Check, CircleAlert, Wrench, X } from "@lucide/svelte";
  import { actions } from "../lib/store";
  import type { Worker } from "../lib/types";

  let { worker }: { worker: Worker } = $props();
  let wontfixId = $state<string | null>(null);
  let reason = $state("");

  $effect(() => {
    worker.id;
    wontfixId = null;
    reason = "";
  });

  function submitWontfix() {
    if (!wontfixId) return;
    actions.wontfixFinding(worker.id, wontfixId, reason);
    if (reason.trim()) {
      wontfixId = null;
      reason = "";
    }
  }
</script>

<section class="findings-panel" aria-label="Review findings">
  <div class="section-heading"><div><p>LEAD REVIEW</p><h3>Findings</h3></div><span>{worker.findings.filter((finding) => finding.status === "open").length} open</span></div>
  {#if worker.findings.length === 0}
    <div class="empty-findings"><Check size={16} /><span>No findings recorded for this worker.</span></div>
  {/if}
  {#each worker.findings as finding (finding.id)}
    <article class="finding" data-status={finding.status}>
      <div class="finding-topline">
        <span class="severity" data-severity={finding.severity}>{finding.severity}</span>
        <code>{finding.file}:{finding.line}</code>
        <span class="finding-status">{finding.status}</span>
      </div>
      <h4>{finding.title}</h4>
      <p>{finding.detail}</p>

      {#if finding.status === "fixed"}
        <div class="resolution"><Check size={12} /><span>Fixed · <code>{finding.fixCommit}</code>{finding.evidence ? ` · ${finding.evidence}` : ""}</span></div>
      {:else if finding.status === "wontfix"}
        <div class="resolution"><X size={12} /><span>Wontfix · {finding.justification}</span></div>
      {:else if worker.status === "fixing" && worker.fixerFindingId === finding.id}
        <div class="finding-actions">
          <button class="button button-primary" aria-label={`Apply mock fixer result: ${finding.title}`} onclick={() => actions.completeFixer(worker.id, finding.id)}><Wrench size={13} /> Apply mock fixer result</button>
          <span>Simulates the fixer commit, affected checks, and lead delta review.</span>
        </div>
      {:else if worker.status === "fixing"}
        <div class="finding-actions"><span>Fixer is working another finding; this one waits for the next pass.</span></div>
      {:else}
        <div class="finding-actions">
          <button class="button button-quiet" aria-label={`Fix tiny finding: ${finding.title}`} title="Lead review_fix · up to 5 files × 10 lines, or one larger single-file edit" onclick={() => actions.fixFinding(worker.id, finding.id)}><Wrench size={13} /> Fix tiny</button>
          <button class="button button-quiet" aria-label={`Spawn fixer: ${finding.title}`} onclick={() => actions.spawnFixer(worker.id, finding.id)}>Spawn fixer</button>
          <button class="text-action" onclick={() => { wontfixId = wontfixId === finding.id ? null : finding.id; reason = ""; }}>Wontfix</button>
        </div>
        {#if wontfixId === finding.id}
          <div class="wontfix-form">
            <label for={`reason-${finding.id}`}>Written justification</label>
            <textarea id={`reason-${finding.id}`} bind:value={reason} rows="2" placeholder="Why is this finding intentionally not being fixed?"></textarea>
            <div><button class="button button-quiet" onclick={() => { wontfixId = null; reason = ""; }}>Cancel</button><button class="button button-primary" onclick={submitWontfix}>Confirm wontfix</button></div>
          </div>
        {/if}
      {/if}
    </article>
  {/each}
  {#if worker.findings.length > 0 && worker.findings.every((finding) => finding.status !== "open")}
    <div class="review-closed"><CircleAlert size={13} /> Review closed · every finding is fixed or justified.</div>
  {/if}
</section>

<style>
  .findings-panel { padding: 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--surface); }
  .section-heading { display: flex; align-items: center; justify-content: space-between; margin-bottom: 14px; }
  .section-heading p { margin: 0 0 3px; color: var(--muted); font-size: 7px; letter-spacing: 0.12em; }
  .section-heading h3 { margin: 0; font-size: 13px; font-weight: 650; }
  .section-heading > span { padding: 4px 7px; border-radius: 6px; background: var(--warning-soft); color: var(--warning); font-size: 8px; }
  .finding { padding: 14px 0; border-top: 1px solid var(--line); }
  .finding-topline { display: flex; align-items: center; gap: 8px; }
  .finding-topline > code { flex: 1; overflow: hidden; color: var(--muted); font-family: var(--font-mono); font-size: 8px; text-overflow: ellipsis; white-space: nowrap; }
  .severity { padding: 3px 5px; border-radius: 5px; font-size: 7px; font-weight: 700; text-transform: uppercase; }
  .severity[data-severity="high"] { background: #f4e4e1; color: var(--danger); }
  .severity[data-severity="low"] { background: var(--surface-soft); color: var(--muted); }
  .finding-status { color: var(--muted); font-size: 8px; }
  .finding h4 { margin: 10px 0 5px; font-size: 11px; font-weight: 650; }
  .finding > p { margin: 0; color: var(--muted); font-family: var(--font-prose); font-size: 13px; line-height: 1.5; }
  .finding-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 7px; margin-top: 12px; }
  .finding-actions > span { flex-basis: 100%; color: var(--muted); font-size: 8px; }
  .text-action { padding: 6px; border: 0; background: transparent; color: var(--muted); cursor: pointer; font-size: 9px; }
  .text-action:hover { color: var(--danger); }
  .resolution { display: flex; align-items: flex-start; gap: 6px; margin-top: 10px; color: var(--accent-strong); font-size: 8px; line-height: 1.5; }
  .resolution :global(svg) { flex: 0 0 auto; margin-top: 1px; }
  .resolution code { font-family: var(--font-mono); }
  .wontfix-form { margin-top: 12px; padding: 12px; border-radius: 10px; background: var(--surface-soft); }
  .wontfix-form label { display: block; margin-bottom: 6px; color: var(--ink-soft); font-size: 9px; font-weight: 600; }
  .wontfix-form textarea { width: 100%; resize: vertical; padding: 8px; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface); color: var(--ink); font-size: 10px; }
  .wontfix-form > div { display: flex; justify-content: flex-end; gap: 7px; margin-top: 8px; }
  .empty-findings { display: flex; align-items: center; gap: 8px; padding: 18px 0; border-top: 1px solid var(--line); color: var(--muted); font-size: 10px; }
  .review-closed { display: flex; align-items: center; gap: 6px; padding-top: 12px; border-top: 1px solid var(--line); color: var(--accent-strong); font-size: 9px; }
</style>
