<script lang="ts">
  import { Check, CircleAlert, Wrench, X } from "@lucide/svelte";
  import { actions } from "../lib/store";
  import type { Finding, Worker } from "../lib/types";

  let { worker }: { worker: Worker } = $props();
  type FormKind = "tiny" | "evidence" | "wontfix";
  let findingId = $state<string | null>(null);
  let formKind = $state<FormKind | null>(null);
  let oldString = $state("");
  let newString = $state("");
  let reason = $state("");

  $effect(() => {
    worker.id;
    findingId = null;
    formKind = null;
    oldString = "";
    newString = "";
    reason = "";
  });

  function openForm(id: string, kind: FormKind) {
    findingId = id;
    formKind = kind;
    oldString = "";
    newString = "";
    reason = "";
  }

  function closeForm() {
    findingId = null;
    formKind = null;
    oldString = "";
    newString = "";
    reason = "";
  }

  async function submitTiny(finding: Finding) {
    if (!oldString || oldString === newString) return;
    await actions.fixFinding(worker.id, finding.id, oldString, newString);
    closeForm();
  }

  async function submitFixed(finding: Finding) {
    if (!reason.trim()) return;
    await actions.resolveFinding(worker.id, finding.id, "fixed", reason, null);
    closeForm();
  }

  async function submitWontfix(finding: Finding) {
    if (!reason.trim()) return;
    await actions.wontfixFinding(worker.id, finding.id, reason);
    closeForm();
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
        <div class="resolution"><Check size={12} /><span>Fixed{#if finding.fixCommit} · <code>{finding.fixCommit}</code>{/if}{finding.evidence ? ` · ${finding.evidence}` : ""}</span></div>
      {:else if finding.status === "wontfix"}
        <div class="resolution"><X size={12} /><span>Wontfix · {finding.justification}</span></div>
      {:else if worker.status === "fixing" && worker.fixerFindingId === finding.id}
        <div class="finding-actions"><span>The tracked worker is fixing this finding. Verify its commit, record the affected checks, and close it with evidence after delta review.</span></div>
      {:else if worker.status === "fixing"}
        <div class="finding-actions"><span>The worker is fixing another finding; this one waits for the next pass.</span></div>
      {:else}
        <div class="finding-actions">
          <button class="button button-quiet" aria-label={`Fix tiny finding: ${finding.title}`} title="One existing non-test source file; no schema, API, or dependency changes" onclick={() => openForm(finding.id, "tiny")}><Wrench size={13} /> Fix tiny</button>
          <button class="button button-quiet" aria-label={`Spawn fixer: ${finding.title}`} onclick={() => actions.spawnFixer(worker.id, finding.id)}>Route to worker</button>
          <button class="text-action" onclick={() => openForm(finding.id, "evidence")}>Close with evidence</button>
          <button class="text-action" onclick={() => openForm(finding.id, "wontfix")}>Wontfix</button>
        </div>
        {#if findingId === finding.id && formKind === "tiny"}
          <div class="finding-form">
            <code>{finding.file}</code>
            <label for={`old-${finding.id}`}>Existing text</label>
            <textarea id={`old-${finding.id}`} bind:value={oldString} rows="3"></textarea>
            <label for={`new-${finding.id}`}>Replacement</label>
            <textarea id={`new-${finding.id}`} bind:value={newString} rows="3"></textarea>
            <div><button class="button button-quiet" onclick={closeForm}>Cancel</button><button class="button button-primary" disabled={!oldString || oldString === newString} onclick={() => submitTiny(finding)}>Apply review_fix</button></div>
          </div>
        {:else if findingId === finding.id && formKind === "evidence"}
          <div class="finding-form">
            <label for={`evidence-${finding.id}`}>Verification evidence</label>
            <textarea id={`evidence-${finding.id}`} bind:value={reason} rows="3" placeholder="Commit and checks that prove this finding is fixed"></textarea>
            <div><button class="button button-quiet" onclick={closeForm}>Cancel</button><button class="button button-primary" disabled={!reason.trim()} onclick={() => submitFixed(finding)}>Close fixed</button></div>
          </div>
        {:else if findingId === finding.id && formKind === "wontfix"}
          <div class="finding-form">
            <label for={`reason-${finding.id}`}>Written justification</label>
            <textarea id={`reason-${finding.id}`} bind:value={reason} rows="2" placeholder="Why is this finding intentionally not being fixed?"></textarea>
            <div><button class="button button-quiet" onclick={closeForm}>Cancel</button><button class="button button-primary" disabled={!reason.trim()} onclick={() => submitWontfix(finding)}>Confirm wontfix</button></div>
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
  .section-heading p { margin: 0 0 3px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.12em; }
  .section-heading h3 { margin: 0; font-size: var(--text-body); font-weight: 650; }
  .section-heading > span { padding: 4px 7px; border-radius: 6px; background: var(--warning-soft); color: var(--warning); font-size: var(--text-meta); }
  .finding { padding: 14px 0; border-top: 1px solid var(--line); }
  .finding-topline { display: flex; align-items: center; gap: 8px; }
  .finding-topline > code { flex: 1; overflow: hidden; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .severity { padding: 3px 5px; border-radius: 5px; font-size: var(--text-meta); font-weight: 700; text-transform: uppercase; }
  .severity[data-severity="high"] { background: #f4e4e1; color: var(--danger); }
  .severity[data-severity="low"] { background: var(--surface-soft); color: var(--muted); }
  .finding-status { color: var(--muted); font-size: var(--text-meta); }
  .finding h4 { margin: 10px 0 5px; font-size: var(--text-body); font-weight: 650; }
  .finding > p { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.5; }
  .finding-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 7px; margin-top: 12px; }
  .finding-actions > span { flex-basis: 100%; color: var(--muted); font-size: var(--text-meta); }
  .text-action { padding: 6px; border: 0; background: transparent; color: var(--muted); cursor: pointer; font-size: var(--text-meta); }
  .text-action:hover { color: var(--danger); }
  .resolution { display: flex; align-items: flex-start; gap: 6px; margin-top: 10px; color: var(--accent-strong); font-size: var(--text-meta); line-height: 1.5; }
  .resolution :global(svg) { flex: 0 0 auto; margin-top: 1px; }
  .resolution code { font-family: var(--font-mono); }
  .finding-form { margin-top: 12px; padding: 12px; border-radius: 10px; background: var(--surface-soft); }
  .finding-form > code { display: block; margin-bottom: 8px; color: var(--muted); font-family: var(--font-mono); font-size: var(--text-meta); }
  .finding-form label { display: block; margin: 8px 0 6px; color: var(--ink-soft); font-size: var(--text-meta); font-weight: 600; }
  .finding-form textarea { width: 100%; resize: vertical; padding: 8px; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface); color: var(--ink); font-family: var(--font-mono); font-size: var(--text-body); }
  .finding-form > div { display: flex; justify-content: flex-end; gap: 7px; margin-top: 8px; }
  .empty-findings { display: flex; align-items: center; gap: 8px; padding: 18px 0; border-top: 1px solid var(--line); color: var(--muted); font-size: var(--text-body); }
  .review-closed { display: flex; align-items: center; gap: 6px; padding-top: 12px; border-top: 1px solid var(--line); color: var(--accent-strong); font-size: var(--text-meta); }
</style>
