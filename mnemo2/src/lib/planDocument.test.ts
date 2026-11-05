import { describe, expect, it } from "vitest";
import { planDetailMarkdown, specCapabilities } from "./planDocument";

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

/** A plan's `## Spec changes` slice as the board renders it. */
const SPEC_CHANGES = [
  "# Plan: Tier ladder",
  "",
  "## Goal",
  "Show the ladder.",
  "",
  "## Spec changes",
  "### tier-ladder",
  "#### ADDED Requirements",
  "##### Requirement: Ladder renders",
  "###### Scenario: Ladder visible",
  "- **WHEN** the page opens **THEN** the ladder shows.",
  "",
  "### dispatch-routing",
  "#### MODIFIED Requirements",
  "",
  "## Steps",
  "- [ ] 1. Build it.",
].join("\n");

describe("specCapabilities", () => {
  it("lists the capabilities a plan's spec changes touch, in order", () => {
    expect(specCapabilities(SPEC_CHANGES)).toEqual(["tier-ladder", "dispatch-routing"]);
  });

  it("drops duplicate capabilities", () => {
    expect(specCapabilities("## Spec changes\n### tier-ladder\n### tier-ladder\n")).toEqual(["tier-ladder"]);
  });

  it("matches the section heading case-insensitively", () => {
    expect(specCapabilities("## SPEC CHANGES\n### tier-ladder\n")).toEqual(["tier-ladder"]);
  });

  it("returns nothing without a Spec changes section", () => {
    expect(specCapabilities(PLAN)).toEqual([]);
  });

  it("ignores headings inside fenced blocks", () => {
    const fenced = ["## Spec changes", "### real-cap", "```", "### fenced-fake", "```"].join("\n");
    expect(specCapabilities(fenced)).toEqual(["real-cap"]);
  });

  it("ignores non-kebab headings like Requirement:", () => {
    const requirements = ["## Spec changes", "### Requirement: Foo bar", "#### ADDED Requirements", "### real-cap"].join("\n");
    expect(specCapabilities(requirements)).toEqual(["real-cap"]);
  });
});
