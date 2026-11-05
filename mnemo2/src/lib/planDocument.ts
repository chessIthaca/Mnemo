/**
 * A plan file (`.coding/plans/<id>.md`) as a plan card's Details renders it.
 *
 * A card already shows the plan title, its goal, and one line per step, so the
 * expanded document is trimmed to what the card does not show: kind, context,
 * acceptance scenarios, decisions, interfaces and progress. The `## Steps`
 * section stays only when the card shows bare step headers, because then the
 * full step text is the extra detail a reader opened the card for.
 */

/** One `## `-headed slice of a plan document; the preamble has no heading. */
interface Section {
  /** The heading text after `## `, or `null` for the text before the first one. */
  heading: string | null;
  lines: string[];
}

/**
 * Split a plan document at its `## ` headings. Headings inside fenced code
 * blocks are content, not structure, so the fence state is tracked.
 */
function splitSections(markdown: string): Section[] {
  const sections: Section[] = [{ heading: null, lines: [] }];
  let fenced = false;
  for (const line of markdown.split("\n")) {
    if (/^\s*```/.test(line)) fenced = !fenced;
    const heading = fenced ? null : /^##\s+(.+?)\s*$/.exec(line);
    if (heading) {
      sections.push({ heading: heading[1], lines: [line] });
      continue;
    }
    // The card carries the title; the document's own `# Plan:` line repeats it.
    if (!fenced && sections.length === 1 && /^#\s+/.test(line)) continue;
    sections[sections.length - 1].lines.push(line);
  }
  return sections;
}

/**
 * The markdown a plan card's Details shows.
 *
 * `keepSteps` is computed from the card's steps: `true` when at least one step
 * shows a header, so the document's full step text adds to what the card
 * already renders.
 */
export function planDetailMarkdown(markdown: string, keepSteps: boolean): string {
  const sections = splitSections(markdown.replace(/\r\n?/g, "\n"));
  const kept = sections.filter((section) => {
    const heading = section.heading?.toLowerCase();
    if (heading === "goal") return false;
    if (heading === "steps" && !keepSteps) return false;
    return true;
  });
  return kept
    .map((section) => section.lines.join("\n").trim())
    .filter((text) => text !== "")
    .join("\n\n");
}

/**
 * The kebab-case capabilities a plan's `## Spec changes` section names, in
 * order, without duplicates — one `### <name>` heading per capability, as
 * the plan file renders its OpenSpec deltas. Requirement and scenario
 * headings, and headings inside fenced code blocks, never make the list.
 */
export function specCapabilities(markdown: string): string[] {
  const sections = splitSections(markdown.replace(/\r\n?/g, "\n"));
  const section = sections.find((candidate) => candidate.heading?.toLowerCase() === "spec changes");
  if (!section) return [];
  const names: string[] = [];
  let fenced = false;
  for (const line of section.lines) {
    if (/^\s*```/.test(line)) fenced = !fenced;
    if (fenced) continue;
    const heading = /^###\s+(.+?)\s*$/.exec(line);
    if (!heading) continue;
    const name = heading[1].trim();
    if (/^[a-z0-9]+(-[a-z0-9]+)*$/.test(name) && !names.includes(name)) names.push(name);
  }
  return names;
}
