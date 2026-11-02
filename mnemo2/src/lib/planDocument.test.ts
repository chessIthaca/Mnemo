import { describe, expect, it } from "vitest";
import { planDetailMarkdown } from "./planDocument";

/** A plan file in the shape `PlanFile::serialize` writes it. */
const PLAN = [
  "# Plan: Plans tab",
  "",
  "## Goal",
  "Show the whole plan.",
  "",
  "## Kind",
  "implementation",
  "",
  "## Context",
  "The card shows only a title and one line per step.",
  "",
  "## Acceptance scenarios",
  "- The top bar shows Plans.",
  "",
  "## Steps",
  "- [ ] 1. Expose the document",
  "- [ ] 2. Render the details",
  "",
  "## Decisions",
  "- Details is lazy.",
  "",
  "## Interfaces",
  "```",
  "## this line is code, not a heading",
  "```",
  "",
  "## Progress",
  "Step 1 is in review.",
].join("\n");

describe("planDetailMarkdown", () => {
  it("drops the title and goal the card already shows", () => {
    const text = planDetailMarkdown(PLAN, true);
    expect(text).not.toContain("# Plan:");
    expect(text).not.toContain("Show the whole plan.");
    expect(text.startsWith("## Kind")).toBe(true);
  });

  it("keeps context, acceptance scenarios, decisions, interfaces and progress", () => {
    const text = planDetailMarkdown(PLAN, true);
    expect(text).toContain("## Context");
    expect(text).toContain("The card shows only a title and one line per step.");
    expect(text).toContain("## Acceptance scenarios");
    expect(text).toContain("## Decisions");
    expect(text).toContain("## Interfaces");
    expect(text).toContain("## this line is code, not a heading");
    expect(text).toContain("## Progress");
  });

  it("keeps the steps only when the card shows bare headers", () => {
    expect(planDetailMarkdown(PLAN, false)).not.toContain("## Steps");
    const withSteps = planDetailMarkdown(PLAN, true);
    expect(withSteps).toContain("## Steps");
    expect(withSteps).toContain("- [ ] 1. Expose the document");
  });

  it("does not split a fenced block that contains a heading line", () => {
    const text = planDetailMarkdown(PLAN, true);
    // The fenced "## this line is code" stays inside Interfaces; Progress follows it.
    expect(text.indexOf("## this line is code")).toBeLessThan(text.indexOf("## Progress"));
    expect(text).toContain("```");
  });

  it("handles CRLF documents and a plan without a goal", () => {
    const text = planDetailMarkdown("# Plan: Bare\r\n\r\n## Context\r\nNo goal here.\r\n", false);
    expect(text).toBe("## Context\nNo goal here.");
  });

  it("returns empty text for a document the card already shows in full", () => {
    expect(planDetailMarkdown("# Plan: Only a title\n\n## Goal\nJust the goal.\n", false)).toBe("");
  });
});
