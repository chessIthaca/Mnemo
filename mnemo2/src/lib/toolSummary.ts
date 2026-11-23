import {
  actionPhrase,
  argPaths,
  basename,
  dedupePaths,
  mnemoMcpTool,
  prettyToolName,
  stringArg,
  teamPhrase,
} from "./toolCardPaths";

function compactHeadline(text: string): string {
  const line = text.split(/\r?\n/, 1)[0].trim();
  return line.length > 120 ? `${line.slice(0, 119).trimEnd()}…` : line;
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
  // A Mnemo tool behind the plugin's MCP server keeps its own phrase; the
  // inner name is what `teamPhrase` knows (`team_status`, `team_dispatch`, …).
  const phrase = (toolName === "write_file" ? "Writing files" : null)
    ?? teamPhrase(mnemoMcpTool(toolName) ?? toolName, args, nameOf)
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