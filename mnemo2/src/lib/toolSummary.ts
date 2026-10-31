import { actionPhrase, prettyToolName } from "../../../frontend/src/lib/toolCardPaths";

/** Team tool headlines: `worker` names the worker the call acts on, when it names one. */
const teamPhrases: Record<string, (worker: string | null) => string> = {
  team_message: (worker) => (worker ? `Messaging ${worker}` : "Messaging a worker"),
  team_status: (worker) => (worker ? `Reading ${worker}’s status` : "Reading team status"),
  team_stop: (worker) => (worker ? `Stopping ${worker}` : "Stopping a worker"),
  team_resume: (worker) => (worker ? `Resuming ${worker}` : "Resuming a worker"),
  team_compact: (worker) => (worker ? `Compacting ${worker}’s context` : "Compacting a worker’s context"),
  team_review: (worker) => (worker ? `Reviewing ${worker}’s work` : "Reviewing a worker"),
  team_check: (worker) => (worker ? `Recording a check for ${worker}` : "Recording a worker check"),
  team_resolve_finding: (worker) => (worker ? `Resolving a finding for ${worker}` : "Resolving a review finding"),
  review_fix: (worker) => (worker ? `Applying a review fix for ${worker}` : "Applying a review fix"),
  team_route_fix: (worker) => (worker ? `Returning a finding to ${worker}` : "Returning a finding to the worker"),
  team_merge: (worker) => (worker ? `Merging ${worker}’s branch` : "Merging a worker branch"),
  team_revert_merge: (worker) => (worker ? `Reverting ${worker}’s merge` : "Reverting a merge"),
  team_stop_all: () => "Stopping all workers",
  message_parent: () => "Sending an update to the lead",
};

function compactHeadline(text: string): string {
  const line = text.split(/\r?\n/, 1)[0].trim();
  return line.length > 120 ? `${line.slice(0, 119).trimEnd()}…` : line;
}

/** One string argument, from complete JSON or from a still-streaming fragment. */
function stringArg(args: string, key: string): string | null {
  try {
    const parsed: unknown = JSON.parse(args);
    if (parsed && typeof parsed === "object" && key in parsed) {
      const value = (parsed as Record<string, unknown>)[key];
      return typeof value === "string" && value.trim() ? value : null;
    }
    return null;
  } catch {
    // An in-flight argument fragment still identifies the real action.
    const marker = `"${key}"`;
    const at = args.indexOf(marker);
    if (at < 0) return null;
    const match = /^\s*:\s*"((?:[^"\\]|\\.)*)"/.exec(args.slice(at + marker.length));
    return match?.[1] ? match[1] : null;
  }
}

function teamPhrase(toolName: string, args: string, nameOf: (workerId: string) => string): string | null {
  const workerId = stringArg(args, "worker_id");
  const worker = workerId ? nameOf(workerId) : null;
  if (toolName !== "team_dispatch") return teamPhrases[toolName]?.(worker) ?? null;
  const brief = stringArg(args, "brief");
  const target = worker ? `Delegating to ${worker}` : "Delegating";
  return brief ? `${target}: ${brief}` : worker ? target : "Delegating a task";
}

/** A compact human headline derived from the actual call, never its outcome. */
export function toolSummary(
  toolName: string,
  args: string,
  nameOf: (workerId: string) => string = (workerId) => workerId,
): string {
  const phrase = (toolName === "write_file" ? "Writing files" : null)
    ?? teamPhrase(toolName, args, nameOf)
    ?? actionPhrase(toolName, args)
    ?? `Using ${prettyToolName(toolName)}`;
  return compactHeadline(phrase);
}
