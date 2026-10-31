import type { Worker } from "./types";

export type EditKind =
  | "source"
  | "new-file"
  | "schema"
  | "api"
  | "dependency"
  | "test";

export interface FileEditScope {
  path: string;
  changedLines: number;
  kind?: EditKind;
}

export interface LeadEditScope {
  files: FileEditScope[];
  crossModule?: boolean;
}

export type MergeEligibility =
  | { ok: true }
  | { ok: false; reason: string };

/** Explain why a lead edit falls outside the tiny-work policy. */
export function leadEditViolation(scope: LeadEditScope): string | null {
  if (scope.files.length === 0) return "No files changed.";
  if (scope.files.length > 5) {
    return "Tiny multi-file work is capped at 5 files; delegate the larger change.";
  }
  if (scope.crossModule) return "Cross-module refactors belong with a worker.";

  for (const file of scope.files) {
    if (!Number.isInteger(file.changedLines) || file.changedLines < 1) {
      return `${file.path}: changed lines must be a positive integer.`;
    }
    if (file.kind && file.kind !== "source") {
      return `${file.path}: ${file.kind} changes require delegation.`;
    }
    if (scope.files.length > 1 && file.changedLines > 10) {
      return `${file.path}: multi-file tiny work allows at most 10 lines per file.`;
    }
  }

  return null;
}

/** Whether the lead may apply this edit without delegating implementation. */
export function isLeadTinyEdit(scope: LeadEditScope): boolean {
  return leadEditViolation(scope) === null;
}

/** One structured review-evidence item that still blocks a merge. */
export interface ReviewEvidenceGap {
  field: string;
  label: string;
}

/** The five base assessments every new standard review must record. */
const BASE_EVIDENCE_FIELDS = [
  ["intent", "Intent"],
  ["execution", "Execution path"],
  ["integration", "Integration"],
  ["testQuality", "Test quality"],
  ["residualRisk", "Residual risk"],
] as const;

/**
 * List the structured review-evidence items still missing for this worker.
 *
 * Mirrors the host gate so the UI never offers a merge the runtime would
 * refuse: a review without evidence (any legacy review) needs a fresh review,
 * a high-risk review additionally needs an independent audit linked at the
 * reviewed tip plus the lead's adjudication.
 */
export function reviewEvidenceGaps(worker: Worker): ReviewEvidenceGap[] {
  const review = worker.review;
  if (!review) return [{ field: "review", label: "A review of the current tip" }];
  const evidence = review.evidence;
  if (!evidence) {
    return [{ field: "evidence", label: "A fresh review with structured evidence" }];
  }
  const gaps: ReviewEvidenceGap[] = [];
  if (!evidence.unanchoredPass) {
    gaps.push({ field: "unanchoredPass", label: "An unanchored diff pass" });
  }
  for (const [field, label] of BASE_EVIDENCE_FIELDS) {
    if (!evidence[field]?.trim()) gaps.push({ field, label });
  }
  if (worker.reviewRisk === "high") {
    if (!evidence.independentReviewerId?.trim()) {
      gaps.push({ field: "independentReviewerId", label: "A linked independent audit" });
    }
    if (!evidence.auditAdjudication?.trim()) {
      gaps.push({ field: "auditAdjudication", label: "The lead's audit adjudication" });
    }
  }
  return gaps;
}

/** Whether this worker is a read-only independent audit rather than an implementer. */
export function isAuditWorker(worker: Worker): boolean {
  return worker.kind === "audit";
}

/** Check the durable review and exact-tip evidence required before merge. */
export function canMerge(worker: Worker): MergeEligibility {
  if (isAuditWorker(worker)) {
    return {
      ok: false,
      reason: "Independent audits never merge; the lead adjudicates the target review.",
    };
  }
  if (worker.status === "cancelled") {
    return { ok: false, reason: "Checkout was cleaned; the retained branch is review-only." };
  }
  if (worker.status === "merged") return { ok: false, reason: "Already merged." };
  if (worker.status !== "in_review") {
    return { ok: false, reason: `Worker is ${worker.status.replace("_", " ")}.` };
  }
  const review = worker.review;
  if (!review) return { ok: false, reason: "The current worker tip has not been reviewed." };

  const openFindings = worker.findings.filter((finding) => finding.status === "open");
  if (openFindings.length > 0) {
    return {
      ok: false,
      reason: `${openFindings.length} review finding${openFindings.length === 1 ? " is" : "s are"} still open.`,
    };
  }

  const unjustified = worker.findings.find(
    (finding) => finding.status === "wontfix" && !finding.justification?.trim(),
  );
  if (unjustified) {
    return { ok: false, reason: `Finding ${unjustified.id} needs a written justification.` };
  }
  const unevidenced = worker.findings.find(
    (finding) => finding.status === "fixed" && !finding.evidence?.trim(),
  );
  if (unevidenced) {
    return { ok: false, reason: `Finding ${unevidenced.id} needs fixed evidence.` };
  }

  if (worker.checks.length === 0) {
    return { ok: false, reason: "No checks have passed on the merge-time tip." };
  }
  const incompleteCheck = worker.checks.find(
    (check) => check.status !== "passed" || check.tip !== review.tip,
  );
  if (incompleteCheck) {
    return { ok: false, reason: `${incompleteCheck.name} has not passed on the merge-time tip.` };
  }

  const gaps = reviewEvidenceGaps(worker);
  if (gaps.length > 0) {
    return { ok: false, reason: `Review evidence is incomplete: ${gaps[0].label.toLowerCase()}.` };
  }

  return { ok: true };
}
