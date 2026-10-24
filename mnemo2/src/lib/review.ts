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

/** Check the review and branch-tip conditions required before a mock merge. */
export function canMerge(worker: Worker): MergeEligibility {
  if (worker.status === "merged") return { ok: false, reason: "Already merged." };
  if (worker.status !== "in_review") {
    return { ok: false, reason: `Worker is ${worker.status.replace("_", " ")}.` };
  }

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

  const incompleteCheck = worker.checks.find((check) => check.status !== "passed");
  if (incompleteCheck) {
    return { ok: false, reason: `${incompleteCheck.name} has not passed on the merge-time tip.` };
  }

  return { ok: true };
}
