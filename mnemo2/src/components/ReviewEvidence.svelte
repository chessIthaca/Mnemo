<script lang="ts">
  import { Link2, ShieldAlert, ShieldCheck } from "@lucide/svelte";
  import { app } from "../lib/store";
  import type { ReviewEvidence, Worker } from "../lib/types";

  let { worker }: { worker: Worker } = $props();

  const review = $derived(worker.review);
  const evidence: ReviewEvidence | undefined = $derived(review?.evidence);
  const highRisk = $derived(worker.reviewRisk === "high");
  const reviewer = $derived(
    evidence?.independentReviewerId
      ? $app.workers.find((item) => item.id === evidence.independentReviewerId)
      : undefined,
  );

  const assessments = $derived([
    { id: "intent", label: "Intent", value: evidence?.intent ?? "" },
    { id: "execution", label: "Execution path", value: evidence?.execution ?? "" },
    { id: "integration", label: "Integration", value: evidence?.integration ?? "" },
    { id: "test-quality", label: "Test quality", value: evidence?.testQuality ?? "" },
    { id: "residual-risk", label: "Residual risk", value: evidence?.residualRisk ?? "" },
  ]);
</script>

<section class="review-evidence" aria-label="Review evidence">
  <div class="evidence-heading">
    <span>REVIEW EVIDENCE</span>
    <div class="evidence-badges">
      <span class="risk-badge" data-risk={worker.reviewRisk}>
        {highRisk ? "High risk" : "Standard risk"}
      </span>
      {#if evidence}
        <span class="pass-badge" data-recorded={evidence.unanchoredPass}>
          {#if evidence.unanchoredPass}
            <ShieldCheck size={11} /> Unanchored pass recorded
          {:else}
            <ShieldAlert size={11} /> Unanchored pass missing
          {/if}
        </span>
      {/if}
    </div>
  </div>

  {#if !review}
    <p class="evidence-note" role="note">The current tip has not been reviewed.</p>
  {:else if !evidence}
    <p class="evidence-note legacy" role="note">
      This review predates structured evidence. Record a fresh review of the exact tip before
      <code>{review.tip}</code> can merge.
    </p>
  {:else}
    <p class="evidence-summary">{review.summary}</p>
    <dl class="evidence-list">
      {#each assessments as assessment (assessment.id)}
        <div class="evidence-row" data-field={assessment.id}>
          <dt>{assessment.label}</dt>
          <dd>{assessment.value.trim() || "Not recorded"}</dd>
        </div>
      {/each}
    </dl>
  {/if}

  {#if highRisk}
    <section class="evidence-audit" aria-label="Independent audit">
      <div class="audit-heading"><span>INDEPENDENT AUDIT</span></div>
      {#if evidence?.independentReviewerId}
        <p class="audit-line">
          <Link2 size={11} />
          <span>{reviewer ? `${reviewer.name} · ${evidence.independentReviewerId}` : evidence.independentReviewerId}</span>
          {#if reviewer?.auditTip}<code>{reviewer.auditTip}</code>{/if}
        </p>
        <p class="audit-adjudication" data-state={evidence.auditAdjudication?.trim() ? "recorded" : "missing"}>
          {evidence.auditAdjudication?.trim() || "The lead has not adjudicated this audit yet."}
        </p>
      {:else}
        <p class="evidence-note" role="note">
          No completed independent audit is linked. A high-risk merge needs a fresh read-only audit
          of this tip and the lead's adjudication.
        </p>
      {/if}
    </section>
  {/if}
</section>

<style>
  .review-evidence { display: grid; gap: 10px; padding: 14px 16px; border: 1px solid var(--line); border-radius: 13px; background: var(--surface); }
  .evidence-heading { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.08em; }
  .evidence-badges { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
  .risk-badge, .pass-badge { display: inline-flex; align-items: center; gap: 4px; padding: 3px 7px; border-radius: 6px; background: var(--surface-soft); color: var(--ink-soft); font-size: var(--text-meta); font-weight: 650; letter-spacing: 0; }
  .risk-badge[data-risk="high"] { background: #f4e4e1; color: var(--danger); }
  .pass-badge[data-recorded="true"] { background: var(--accent-soft); color: var(--accent-strong); }
  .pass-badge[data-recorded="false"] { background: var(--warning-soft); color: var(--warning); }
  .evidence-summary { margin: 0; color: var(--ink-soft); font-size: var(--text-body); line-height: 1.5; }
  .evidence-note { margin: 0; color: var(--muted); font-size: var(--text-meta); line-height: 1.5; }
  .evidence-note.legacy { color: var(--warning); }
  .evidence-note code { font-family: var(--font-mono); font-size: var(--text-meta); }
  .evidence-list { display: grid; gap: 8px; margin: 0; }
  .evidence-row { display: grid; grid-template-columns: 96px 1fr; gap: 10px; }
  .evidence-row dt { color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.04em; text-transform: uppercase; }
  .evidence-row dd { margin: 0; color: var(--ink-soft); font-size: var(--text-meta); line-height: 1.5; }
  .evidence-audit { display: grid; gap: 6px; margin-top: 2px; padding-top: 9px; border-top: 1px solid var(--line); }
  .audit-heading { color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.08em; }
  .audit-line { display: flex; flex-wrap: wrap; align-items: center; gap: 5px; margin: 0; color: var(--ink-soft); font-size: var(--text-meta); }
  .audit-line code { padding: 2px 5px; border-radius: 5px; background: var(--surface-soft); font-family: var(--font-mono); font-size: var(--text-meta); }
  .audit-adjudication { margin: 0; color: var(--ink-soft); font-size: var(--text-meta); line-height: 1.5; }
  .audit-adjudication[data-state="missing"] { color: var(--warning); }
</style>
