import { describe, expect, it } from "vitest";
import { createWorkers } from "../test/fixtures";
import { canMerge, isLeadTinyEdit, leadEditViolation } from "./review";

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
});
