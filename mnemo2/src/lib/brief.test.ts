import { describe, expect, it } from "vitest";
import { briefSummary } from "./brief";

const leadBrief = [
  "# Plan: Short worker summaries",
  "",
  "Goal: Mission control shows each worker's task as **at most three lines**.",
  "",
  "- add a `briefSummary` helper in mnemo2/src/lib/brief.ts",
  "- clamp the row summary with CSS",
  "",
  "---",
  "",
  "Verify with *npm test* and `npm run check`.",
].join("\n");

describe("briefSummary", () => {
  it("condenses a multi-paragraph Markdown brief into one plain paragraph", () => {
    expect(briefSummary(leadBrief)).toBe(
      "Plan: Short worker summaries Goal: Mission control shows each worker's task as at most three lines. " +
        "add a briefSummary helper in mnemo2/src/lib/brief.ts clamp the row summary with CSS " +
        "Verify with npm test and npm run check.",
    );
  });

  it("drops the heading marker of an audit brief, keeping its lead line first", () => {
    const auditBrief = [
      "# Independent read-only audit",
      "",
      "Read the worker's diff at the inspected tip and report findings to the lead.",
    ].join("\n");
    const summary = briefSummary(auditBrief);
    expect(summary.startsWith("Independent read-only audit")).toBe(true);
    expect(summary).toBe(
      "Independent read-only audit Read the worker's diff at the inspected tip and report findings to the lead.",
    );
  });

  it("returns an empty string for an empty brief", () => {
    expect(briefSummary("")).toBe("");
  });

  it("leaves a one-line plain brief unchanged", () => {
    expect(briefSummary("Team state and review flow")).toBe("Team state and review flow");
  });
});