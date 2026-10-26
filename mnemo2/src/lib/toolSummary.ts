import { actionPhrase, prettyToolName } from "../../../frontend/src/lib/toolCardPaths";

const teamPhrases: Record<string, string> = {
  team_message: "Messaging a worker",
  team_status: "Reading team status",
  team_review: "Reviewing a worker",
  team_check: "Recording a worker check",
  team_resolve_finding: "Resolving a review finding",
  review_fix: "Applying a review fix",
  team_route_fix: "Returning a finding to the worker",
  team_merge: "Merging a worker branch",
  team_revert_merge: "Reverting a merge",
  team_stop_all: "Stopping all workers",
  message_parent: "Sending an update to the lead",
};

function compactHeadline(text: string): string {
  const line = text.split(/\r?\n/, 1)[0].trim();
  return line.length > 120 ? `${line.slice(0, 119).trimEnd()}…` : line;
}

function teamPhrase(toolName: string, args: string): string | null {
  if (toolName !== "team_dispatch") return teamPhrases[toolName] ?? null;
  try {
    const parsed: unknown = JSON.parse(args);
    if (parsed && typeof parsed === "object" && "brief" in parsed) {
      const brief = (parsed as { brief?: unknown }).brief;
      if (typeof brief === "string" && brief.trim()) return `Delegating: ${brief}`;
    }
  } catch {
    // An in-flight argument fragment still identifies the real action.
  }
  return "Delegating a task";
}

/** A compact human headline derived from the actual call, never its outcome. */
export function toolSummary(toolName: string, args: string): string {
  const phrase = (toolName === "write_file" ? "Writing files" : null)
    ?? teamPhrase(toolName, args)
    ?? actionPhrase(toolName, args)
    ?? `Using ${prettyToolName(toolName)}`;
  return compactHeadline(phrase);
}
