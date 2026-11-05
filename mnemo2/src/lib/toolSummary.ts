import { actionPhrase, argPaths, basename, dedupePaths, prettyToolName } from "../../../frontend/src/lib/toolCardPaths";

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

/** The verb a file tool's headline prefixes its file names with, or null for
 *  every other tool. `file_write` appends when its `mode` argument says so;
 *  `write_file` is `file_write`'s legacy alias, `file_append` the pre-mode
 *  append tool. */
function fileVerb(toolName: string, args: string): string | null {
  if (toolName === "read_files") return "Reading";
  if (toolName === "file_edit" || toolName === "multi_edit") return "Editing";
  if (toolName === "file_append") return "Appending to";
  if (toolName === "file_write" || toolName === "write_file") {
    return stringArg(args, "mode") === "append" ? "Appending to" : "Writing";
  }
  return null;
}

/** The string literals a still-streaming `"files"` array names, scanned from
 *  `from` (just past its `[`): bare string entries (`"a.rs"`) and the values
 *  of `"path"`/`"file"` keys — never the `old_string`/`new_string` payloads
 *  of a nested ops anchor. A literal followed by `:` is a key, not a value;
 *  an unfinished literal is never shown half-typed (stringArg's rule). */
function fileArrayLiterals(fragment: string, from: number): string[] {
  const found: string[] = [];
  let at = from;
  let depth = 1; // inside the files array itself
  let key: string | null = null;
  while (at < fragment.length) {
    const char = fragment[at];
    if (char === '"') {
      const literal = /^"((?:[^"\\]|\\.)*)"/.exec(fragment.slice(at));
      if (literal === null) break; // cut mid-string: keep the complete literals only
      at += literal[0].length;
      if (/^\s*:/.test(fragment.slice(at))) key = literal[1];
      else {
        if ((depth === 1 || key === "path" || key === "file") && literal[1] !== "") found.push(literal[1]);
        key = null;
      }
      continue;
    }
    if (char === "{" || char === "[") {
      depth += 1;
      key = null;
    } else if (char === "}" || char === "]") {
      depth -= 1;
      key = null;
      if (depth < 1) break; // the files array closed
    }
    at += 1; // commas, colons and whitespace between the literals
  }
  return found;
}

/** The paths a file tool's still-streaming argument fragment names: every
 *  complete `"path"`/`"file"` object value, plus the entries of its `"files"`
 *  array. argPaths' own fragment salvage returns only the first literal; this
 *  one keeps collecting while the arguments stream in. */
function salvagedPaths(fragment: string): string[] {
  const found: string[] = [];
  for (const match of fragment.matchAll(/"(?:path|file)"\s*:\s*"((?:[^"\\]|\\.)*)"/g)) {
    found.push(match[1]);
  }
  const array = /"files"\s*:\s*\[/.exec(fragment);
  if (array !== null) {
    found.push(...fileArrayLiterals(fragment, array.index + array[0].length));
  }
  return found;
}

/** The project-relative paths a file tool's arguments name — complete JSON
 *  parsed properly, a still-streaming fragment salvaged — deduplicated. */
function headlinePaths(toolName: string, args: string): string[] {
  try {
    JSON.parse(args);
    return dedupePaths(argPaths(args, toolName));
  } catch {
    return dedupePaths(salvagedPaths(args));
  }
}

/** "Reading toolSummary.ts, types.ts": the verb plus the first three file
 *  basenames, `+N more` after that, with the full paths as the row tooltip. */
function namedFilesPhrase(verb: string, paths: string[]): { text: string; title: string } {
  const names = paths.map(basename);
  const extra = names.length > 3 ? ` +${names.length - 3} more` : "";
  return {
    text: compactHeadline(`${verb} ${names.slice(0, 3).join(", ")}${extra}`),
    title: paths.join(", "),
  };
}

/** A compact human headline derived from the actual call, never its outcome.
 *  File tools name the files they touch ("Reading a.rs, b.rs") instead of
 *  counting them; `title` carries the full paths for the row's tooltip. */
export function toolHeadline(
  toolName: string,
  args: string,
  nameOf: (workerId: string) => string = (workerId) => workerId,
): { text: string; title: string } {
  const verb = fileVerb(toolName, args);
  if (verb !== null) {
    const paths = headlinePaths(toolName, args);
    if (paths.length > 0) return namedFilesPhrase(verb, paths);
  }
  const phrase = (toolName === "write_file" ? "Writing files" : null)
    ?? teamPhrase(toolName, args, nameOf)
    ?? actionPhrase(toolName, args)
    ?? `Using ${prettyToolName(toolName)}`;
  const text = compactHeadline(phrase);
  return { text, title: text };
}

/** The headline text alone — `toolHeadline` minus the tooltip. */
export function toolSummary(
  toolName: string,
  args: string,
  nameOf: (workerId: string) => string = (workerId) => workerId,
): string {
  return toolHeadline(toolName, args, nameOf).text;
}