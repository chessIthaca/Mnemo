import { describe, expect, it } from "vitest";
import { createAuditWorker, createHighRiskWorker, createWorkers } from "../test/fixtures";
import {
  canMerge,
  isAuditWorker,
  isLeadTinyEdit,
  leadEditViolation,
  reviewEvidenceGaps,
} from "./review";
import type { Worker } from "./types";

const files = (count: number, changedLines = 10) =>
  Array.from({ length: count }, (_, index) => ({
    path: `src/file-${index + 1}.ts`,
    changedLines,
    kind: "source" as const,
  }));

describe("lead tiny-work policy", () => {
  it("allows five files with ten changed lines each", () => {
    expect(isLeadTinyEdit({ files: files(5) })).toBe(true);
  });

  it("allows a larger edit when it is confined to one file", () => {
    expect(isLeadTinyEdit({ files: files(1, 96) })).toBe(true);
  });

  it("rejects six files even when every edit is small", () => {
    expect(leadEditViolation({ files: files(6, 1) })).toContain("5 files");
  });

  it("rejects more than ten lines in one file of a multi-file edit", () => {
    const edit = files(2);
    edit[1].changedLines = 11;
    expect(leadEditViolation({ files: edit })).toContain("10 lines");
  });

  it("rejects excluded scopes even for a single-file edit", () => {
    expect(
      isLeadTinyEdit({
        files: [{ path: "package.json", changedLines: 2, kind: "dependency" }],
      }),
    ).toBe(false);
    expect(isLeadTinyEdit({ files: files(1, 4), crossModule: true })).toBe(false);
  });
});

describe("merge eligibility", () => {
  it("accepts a reviewed tip only after checks and findings are closed", () => {
    const ana = createWorkers().find((worker) => worker.id === "ana")!;
    expect(canMerge(ana).ok).toBe(false);

    const ready = {
      ...ana,
      findings: ana.findings.map((finding) => ({
        ...finding,
        status: "fixed" as const,
        evidence: "verified on the fix commit",
      })),
    };
    expect(canMerge(ready)).toEqual({ ok: true });

    const stale = {
      ...ready,
      checks: [{ name: "workspace tests", status: "running" as const, detail: "running" }],
    };
    expect(canMerge(stale).ok).toBe(false);
    expect(canMerge({ ...ready, checks: [] }).ok).toBe(false);
    expect(canMerge({
      ...ready,
      checks: ready.checks.map((check) => ({ ...check, tip: "older-tip" })),
    }).ok).toBe(false);
  });

  it("does not merge a cleaned branch without its checkout", () => {
    const ready = createWorkers().find((worker) => worker.id === "ana")!;
    ready.findings = ready.findings.map((finding) => ({
      ...finding,
      status: "fixed" as const,
      evidence: "verified on the fix commit",
    }));
    expect(canMerge({ ...ready, status: "cancelled" })).toEqual({
      ok: false,
      reason: "Checkout was cleaned; the retained branch is review-only.",
    });
  });

  it("requires written justification for every wontfix finding", () => {
    const ana = createWorkers().find((worker) => worker.id === "ana")!;
    const ready = {
      ...ana,
      findings: ana.findings.map((finding) => ({
        ...finding,
        status: "wontfix" as const,
      })),
    };
    expect(canMerge(ready).ok).toBe(false);
  });

  const closed = (worker: Worker): Worker => ({
    ...worker,
    findings: worker.findings.map((finding) => ({
      ...finding,
      status: "fixed" as const,
      evidence: "verified on the fix commit",
    })),
  });

  it("refuses a merge when the review carries no structured evidence", () => {
    const ana = createWorkers().find((worker) => worker.id === "ana")!;
    const legacy: Worker = {
      ...closed(ana),
      review: { summary: "legacy review", tip: ana.review!.tip, reviewedAtMs: 1 },
    };
    const eligibility = canMerge(legacy);
    expect(eligibility.ok).toBe(false);
    expect(eligibility.ok === false && eligibility.reason).toMatch(/structured evidence/i);
  });

  it("refuses a merge until every base assessment and the unanchored pass are recorded", () => {
    const ana = createWorkers().find((worker) => worker.id === "ana")!;
    const evidence = ana.review!.evidence!;
    const cases: Array<[string, typeof evidence]> = [
      ["unanchoredPass", { ...evidence, unanchoredPass: false }],
      ["intent", { ...evidence, intent: "  " }],
      ["execution", { ...evidence, execution: "" }],
      ["integration", { ...evidence, integration: "" }],
      ["testQuality", { ...evidence, testQuality: "" }],
      ["residualRisk", { ...evidence, residualRisk: "" }],
    ];
    for (const [field, partial] of cases) {
      const worker: Worker = {
        ...closed(ana),
        review: { ...ana.review!, evidence: partial },
      };
      expect(reviewEvidenceGaps(worker).some((gap) => gap.field === field)).toBe(true);
      expect(canMerge(worker).ok).toBe(false);
    }
    expect(reviewEvidenceGaps(closed(ana))).toEqual([]);
    expect(canMerge(closed(ana))).toEqual({ ok: true });
  });

  it("refuses a high-risk merge without a linked audit and the lead adjudication", () => {
    const mara = createHighRiskWorker();
    expect(canMerge(mara)).toEqual({ ok: true });

    const unaudited: Worker = {
      ...mara,
      review: {
        ...mara.review!,
        evidence: {
          ...mara.review!.evidence!,
          independentReviewerId: undefined,
          auditAdjudication: undefined,
        },
      },
    };
    const gaps = reviewEvidenceGaps(unaudited).map((gap) => gap.field);
    expect(gaps).toEqual(["independentReviewerId", "auditAdjudication"]);
    expect(canMerge(unaudited).ok).toBe(false);

    const judged: Worker = {
      ...mara,
      review: {
        ...mara.review!,
        evidence: { ...mara.review!.evidence!, auditAdjudication: "   " },
      },
    };
    expect(canMerge(judged).ok).toBe(false);
  });

  it("never merges an audit worker, whatever its review state", () => {
    const audit = createAuditWorker();
    expect(isAuditWorker(audit)).toBe(true);
    expect(canMerge(audit)).toEqual({
      ok: false,
      reason: "Independent audits never merge; the lead adjudicates the target review.",
    });
    expect(canMerge({ ...audit, status: "in_review" }).ok).toBe(false);
  });
});
