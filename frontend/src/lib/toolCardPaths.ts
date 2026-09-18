// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Pure path/label extraction from a tool call's JSON args — drives the
 * ToolCard header's clickable file-name links (Message.tsx) — plus
 * readable-IO parsing for the expanded call-detail body: shell calls
 * (`shellCallFromArgs` / `parseShellOutput`: command block, stdout/stderr
 * split, exit-code chip; backlog 8c1d8a47), file_edit diffs
 * (`fileEditDiff`), and read_files line ranges (`parseReadFilesSections`).
 *
 * Kept React-free so the node-env unit tests can exercise it directly.
 */

import type { ToolResult } from "./types";

/** The basename of a path (forward/back-slash tolerant). */
export function basename(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || path;
}

/**
 * First `"path"` / `"file"` string literal embedded in raw args text.
 *
 * Salvage for TRUNCATED args: `ToolInvocation.args` streams in via
 * `tool_call_arg_delta` fragments, so an interrupted/aborted call can leave
 * JSON cut off mid-object — unparseable forever, while the meaningful value
 * sits intact inside the fragment. Extracts just the literal (no mapping —
 * the label-only-tool exclusions are enforced by {@link argPaths} before
 * this salvage path is ever reached).
 */
function salvagePathLiteral(args: string): string | null {
  const match = /"(?:path|file)"\s*:\s*"([^"]+)"/.exec(args);
  return match ? match[1] : null;
}

/**
 * Extract the full project-relative path(s) referenced by a tool call's args.
 *
 * Only fields that genuinely carry a *file* are returned:
 * - top-level `path` / `file` (file_read, file_write, file_edit, …),
 * - the `read_files` `files[].path` array (batched reads),
 * - `write_review_report`'s `path` (a file under .coding/reviews/).
 *
 * Everything else (shell `purpose`, git `subcommand`, search `pattern`,
 * spawn_agent `name`, skill `skill`, backlog_add `text`) is a label, not a
 * file — returns [] so the header doesn't render a bogus link.
 *
 * Truncated-args salvage: when the JSON cannot be parsed at all (an
 * interrupted stream cut the args off mid-object), {@link salvagePathLiteral}
 * recovers the first embedded `"path"`/`"file"` literal so the card keeps a
 * clickable name instead of going permanently nameless. Returns [] when
 * nothing recoverable exists.
 */
export function argPaths(args: string, toolName?: string): string[] {
  // Tools whose key arg is a label, not a file path — excluded BEFORE
  // parsing so the truncated-args salvage below can never hand one of them
  // a link chip.
  if (
    toolName === "shell" ||
    toolName === "spawn_agent" ||
    toolName === "skill_start" ||
    // skill_create's `name` is a SKILL name (the file stem under
    // .coding/skills/) and its `prompt` carries free text — never an openable
    // file. Excluded like skill_start above, so the truncated-args salvage can
    // never hand the card a bogus link chip. (skill_reload takes no arguments,
    // so it needs no entry: its args object is always empty.)
    toolName === "skill_create" ||
    toolName === "git" ||
    // git_read's `path` param is a log/diff FILTER (often a directory),
    // not an openable file — the op chip (`log -8`, `show d31b606`) is the
    // meaningful part and renders via argLabel instead.
    toolName === "git_read" ||
    toolName === "search" ||
    toolName === "search_read" ||
    // backlog_add's `text` arg is the item's title (a label), not a file
    // path — its chip renders via argLabel, never a file-link.
    toolName === "backlog_add"
  ) {
    return [];
  }
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(args);
  } catch {
    const salvaged = salvagePathLiteral(args);
    return salvaged !== null ? [salvaged] : [];
  }
  // Batched reads: one path per spec. The Rust tool also accepts a single-file
  // top-level `path` shorthand (absorbed from the old file_read tool); when
  // `files` is absent/not-an-array, fall through to the common path/file
  // fallback below so that shape still yields a clickable link.
  if (toolName === "read_files") {
    const files = parsed.files;
    if (Array.isArray(files)) {
      return files
        .map((f) =>
          f && typeof f === "object" && typeof (f as Record<string, unknown>).path === "string"
            ? (f as Record<string, string>).path
            : null,
        )
        .filter((p): p is string => !!p);
    }
  }
  // write_review_report takes a BARE filename — the tool writes it under
  // .coding/reviews/ (subdirectories and ".." are rejected). Map the bare name
  // to the real project-relative location so the link opens the actual file.
  if (toolName === "write_review_report") {
    const path = typeof parsed.path === "string" ? parsed.path : null;
    if (!path) return [];
    return [path.includes("/") || path.includes("\\") ? path : `.coding/reviews/${path}`];
  }
  // Common path-bearing fields.
  const path =
    (typeof parsed.path === "string" && parsed.path) ||
    (typeof parsed.file === "string" && parsed.file) ||
    null;
  return path ? [path] : [];
}

/** A file path plus the 1-based line a `read_files` spec started reading at
 *  (null = not applicable) — chips deep-link the Files viewer to that line. */
export interface PathLine {
  path: string;
  line: number | null;
}

/**
 * Extract the file paths referenced by a tool call's args, each paired with
 * the 1-based line a `read_files` spec started at (`start_line ?? 1`) so its
 * chip can deep-link the Files viewer to the line the read started at.
 * Non-read tools return their `argPaths` paths with `line: null`.
 *
 * React-free so the node-env unit tests can exercise it directly.
 */
export function argPathLines(args: string, toolName?: string): PathLine[] {
  if (toolName !== "read_files") {
    return argPaths(args, toolName).map((path) => ({ path, line: null }));
  }
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(args);
  } catch {
    // Malformed JSON: fall back to argPaths' literal-salvage extraction.
    return argPaths(args, toolName).map((path) => ({ path, line: null }));
  }
  const lineOf = (spec: Record<string, unknown>): number | null =>
    typeof spec.start_line === "number" && Number.isFinite(spec.start_line) && spec.start_line >= 1
      ? Math.floor(spec.start_line)
      : 1;
  const files = parsed.files;
  if (Array.isArray(files)) {
    const out: PathLine[] = [];
    for (const f of files) {
      if (f !== null && typeof f === "object" && typeof (f as Record<string, unknown>).path === "string") {
        out.push({
          path: (f as Record<string, unknown>).path as string,
          line: lineOf(f as Record<string, unknown>),
        });
      }
    }
    return out;
  }
  // Single-file shorthand: top-level path (+ optional start_line).
  const path = typeof parsed.path === "string" ? parsed.path : null;
  return path !== null ? [{ path, line: lineOf(parsed) }] : [];
}

/** A header chip for a ToolCard: a clickable file link (`path !== null`), a
 *  clickable external-URL link (`url` set — web_fetch's fetched page), or a
 *  plain-text label (`path === null`, e.g. an overflow `+N`). */
export interface ToolCardChip {
  key: string;
  text: string;
  path: string | null;
  /** The 1-based line a read_files chip's read started at — deep-links the
   *  Files viewer to that line (null = top of file / not applicable). */
  line: number | null;
  /** Render the label as inline markdown (bold/code, truncation-safe) — set
   *  only on backlog_add's argLabel chip, which carries the queued item's
   *  title (plan 2026-12). Every other pathless chip stays plain text. */
  md?: boolean;
  /** An external http(s) URL the chip opens in the user's default browser —
   *  set only on web_fetch's argLabel chip (via `webFetchUrl`), whose label
   *  is the fetched page's URL. null/undefined = no external link. */
  url?: string | null;
}

/** Normalize a path for dedup: forward slashes, no trailing slash, lowercased
 *  (case-insensitive filesystems + Windows/POSIX separator tolerance). */
function normalizePath(p: string): string {
  return p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

/** Deduplicate a list of file paths by normalized path, keeping the FIRST
 *  occurrence verbatim (so its original casing/separator is the one rendered
 *  and linked). Two paths that differ only by separator or casing collapse to
 *  one; two DISTINCT files that merely share a basename both survive.
 *
 *  React-free so the node-env unit tests can exercise it directly. */
export function dedupePaths(paths: ReadonlyArray<string>): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const p of paths) {
    const norm = normalizePath(p);
    if (!seen.has(norm)) {
      seen.add(norm);
      out.push(p);
    }
  }
  return out;
}

/** Make every chip display name unique IN PLACE: entries whose rendered
 *  basenames collide (two distinct files named e.g. openai.rs read together)
 *  get progressively qualified with parent-directory segments
 *  (`openai.rs` → `provider/openai.rs` → full path) until all texts differ.
 *  Only colliding entries change; singletons keep their bare basename, and
 *  every entry keeps its own full link `path`.
 *
 *  Bounded, so it terminates for every input: qualification is capped at
 *  the longest path's full segment sequence, and a group that STILL collides
 *  at full depth — possible for paths like `/src/main.rs` vs `src/main.rs`
 *  or `src//util.ts` vs `src/util.ts`, which normalizePath treats as
 *  distinct although their segment sequences coincide — falls back to the
 *  raw paths, which are pairwise distinct whenever the normalized paths are.
 *
 *  React-free so the node-env unit tests can exercise it directly. */
function disambiguateDuplicateTexts(entries: { text: string; path: string }[]): void {
  if (entries.length < 2) return;
  const segmentsOf = (p: string): string[] =>
    p
      .replace(/\\/g, "/")
      .split("/")
      .filter((s) => s.length > 0);
  // Group entry indexes sharing one case-insensitive basename.
  const groups = new Map<string, number[]>();
  entries.forEach((e, i) => {
    const key = normalizePath(basename(e.path));
    const group = groups.get(key);
    if (group) group.push(i);
    else groups.set(key, [i]);
  });
  const maxDepth = Math.max(...entries.map((e) => segmentsOf(e.path).length));
  let segs = 2;
  for (; segs <= maxDepth; segs += 1) {
    let collided = false;
    for (const idxs of groups.values()) {
      if (idxs.length < 2) continue;
      const seenTexts = new Set<string>();
      for (const i of idxs) {
        const parts = segmentsOf(entries[i].path);
        const qualified = parts.slice(-Math.min(segs, parts.length)).join("/");
        if (seenTexts.has(qualified.toLowerCase())) {
          collided = true;
          break;
        }
        seenTexts.add(qualified.toLowerCase());
        entries[i].text = qualified;
      }
    }
    if (!collided) return;
  }
  // Still colliding at full depth: same-segment-sequence variants of one
  // basename (leading-slash / interior-double-slash paths). Raw paths are
  // pairwise distinct whenever the normalized paths are — render them.
  for (const idxs of groups.values()) {
    if (idxs.length < 2) continue;
    for (const i of idxs) entries[i].text = entries[i].path;
  }
}

/** Build the file-path chips for a ToolCard header from its grouped calls,
 *  deduplicated so each file appears at most once. Paths are collected across
 *  ALL calls (the same file read in multiple calls → one chip), deduped by
 *  normalized path (first occurrence wins, keeping its link), then capped at 3
 *  with a `+N` overflow chip. Display names are made unique: when deduped
 *  paths share a basename, colliding chips are qualified with parent dirs
 *  (see {@link disambiguateDuplicateTexts}) so the header never prints the
 *  same name twice. read_files chips carry the 1-based line the read started
 *  at (`line`, via {@link argPathLines}) so the link opens the file there.
 *  Returns `[]` for label-only tools (git, shell, search) whose calls carry
 *  no file path — those render a label chip via `argLabel` in Message.tsx
 *  instead.
 *
 *  React-free so the node-env unit tests can exercise it directly. */
export function buildPathChips(
  calls: ReadonlyArray<{ id: string; args: string }>,
  name: string,
): ToolCardChip[] {
  const entries: PathLine[] = [];
  for (const c of calls) {
    entries.push(...argPathLines(c.args, name));
  }
  // First occurrence's line wins per normalized path (mirrors dedupePaths'
  // first-wins dedup below).
  const lineByPath = new Map<string, number | null>();
  for (const e of entries) {
    const norm = normalizePath(e.path);
    if (!lineByPath.has(norm)) lineByPath.set(norm, e.line);
  }
  const deduped = dedupePaths(entries.map((e) => e.path));
  const chips: ToolCardChip[] = [];
  const shown = deduped.slice(0, 3).map((p) => ({ text: basename(p), path: p }));
  disambiguateDuplicateTexts(shown);
  for (const e of shown) {
    chips.push({
      key: `path:${normalizePath(e.path)}`,
      text: e.text,
      path: e.path,
      line: lineByPath.get(normalizePath(e.path)) ?? null,
    });
  }
  if (deduped.length > 3) {
    chips.push({ key: "path:overflow", text: `+${deduped.length - 3}`, path: null, line: null });
  }
  return chips;
}

/** The name segment of a `file::name::line` symbol id; a plain name (or an
 *  id without the three `::`-separated parts) is returned as-is. */
function shortSymbolRef(idOrName: string | null): string | null {
  if (idOrName === null) return null;
  const parts = idOrName.split("::");
  return parts.length === 3 ? parts[1] : idOrName;
}

/**
 * One-line label for the graph_* tools' ToolCard header — WHAT was looked up
 * in the code knowledge graph, so the card reads `Graph Search
 * ("GraphView")` instead of a bare `graph_search`:
 *
 * - `graph_search` → the `query`, quoted (mirroring search's pattern chip),
 * - `graph_context` / `graph_impact` → `name` when present, else the `id`
 *   shortened to its name segment (`file::name::line` → `name`),
 * - `graph_path` → `from → to` with the same id shortening.
 *
 * Returns `null` for any other tool name (the caller falls through to the
 * generic label logic), and for malformed JSON or missing fields.
 * React-free so the node-env unit tests can exercise it directly.
 */
export function graphCallLabel(toolName: string, argsJson: string): string | null {
  if (
    toolName !== "graph_search" &&
    toolName !== "graph_context" &&
    toolName !== "graph_impact" &&
    toolName !== "graph_path"
  ) {
    return null;
  }
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(argsJson);
  } catch {
    return null;
  }
  const str = (v: unknown): string | null =>
    typeof v === "string" && v.trim() ? v.trim() : null;

  if (toolName === "graph_search") {
    const query = str(parsed.query);
    return query === null ? null : `"${query}"`;
  }
  if (toolName === "graph_context" || toolName === "graph_impact") {
    return str(parsed.name) ?? shortSymbolRef(str(parsed.id));
  }
  // graph_path — both endpoints must be present to be meaningful.
  const from = shortSymbolRef(str(parsed.from));
  const to = shortSymbolRef(str(parsed.to));
  return from !== null && to !== null ? `${from} → ${to}` : null;
}

/**
 * One-line label for a `memory_search` call's args — WHAT was searched, so
 * the memory activity card reads `memory_search "resize seam" · bug`
 * instead of a bare `memory_search` (mirroring search's pattern chip).
 *
 * Format: the `query` quoted, then ` · `-separated scope suffixes for the
 * narrowing filters that were set — `record_type` (or legacy `tier`),
 * `prefix`, and `limit N`. A query-less browse with filters reads
 * `browse · …`. Returns `null` for malformed JSON or a call with neither
 * query nor filters (nothing meaningful to show).
 *
 * React-free so the node-env unit tests can exercise it directly.
 */
export function memorySearchLabel(argsJson: string): string | null {
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(argsJson);
  } catch {
    return null;
  }
  const str = (v: unknown): string | null =>
    typeof v === "string" && v.trim() ? v.trim() : null;
  const query = str(parsed.query);
  const scope: string[] = [];
  const recordType = str(parsed.record_type);
  const tier = str(parsed.tier);
  if (recordType !== null) scope.push(recordType);
  else if (tier !== null) scope.push(tier);
  const prefix = str(parsed.prefix);
  if (prefix !== null) scope.push(prefix);
  if (typeof parsed.limit === "number" && Number.isFinite(parsed.limit)) {
    scope.push(`limit ${parsed.limit}`);
  }
  if (query === null) {
    // Query-less browse: still show the filters when any were set.
    return scope.length > 0 ? `browse · ${scope.join(" · ")}` : null;
  }
  return scope.length > 0 ? `"${query}" · ${scope.join(" · ")}` : `"${query}"`;
}

/** The parsed result metadata of a `search` / `search_read` call — which
 *  engine served the query, how many matches it reported, and whether the
 *  broken-regex → literal fallback fired. */
export interface SearchResultInfo {
  /** Which engine served the query (`index` = FTS content index). */
  engine: "index" | "walk";
  /** Reported match count (0 for "no matches found"). */
  matches: number;
  /** Distinct matched files, when the summary reports them. */
  files: number | null;
  /** The literal-fallback note (broken regex matched literally), if present. */
  note: string | null;
}

/**
 * Parse the result metadata out of a `search` / `search_read` tool output —
 * drives the ToolCard's engine + match-count chip and the fallback-note
 * line (Message.tsx).
 *
 * Both tools emit a summary line carrying `engine: index` (the FTS content
 * index) or `engine: walk` (the tree walk), either as
 * `N matches in M files (…engine: E…)` or `no matches found (…engine: E…)`,
 * and both prepend `note: …` when a broken regex was retried as literal
 * text. Returns `null` for output that carries no engine marker (an error
 * result, or output from before the engine field existed).
 *
 * React-free so the node-env unit tests can exercise it directly.
 */
export function searchResultInfo(output: string): SearchResultInfo | null {
  let rest = output;
  let note: string | null = null;
  if (rest.startsWith("note: ")) {
    const nl = rest.indexOf("\n");
    const first = nl === -1 ? rest : rest.slice(0, nl);
    const text = first.slice("note: ".length).trim();
    note = text === "" ? null : text;
    rest = nl === -1 ? "" : rest.slice(nl + 1);
  }
  const engine = /\bengine: (index|walk)\b/.exec(rest);
  if (!engine) return null;
  const counts = /(\d+) matches in (\d+) files/.exec(rest);
  if (counts) {
    return {
      engine: engine[1] as "index" | "walk",
      matches: parseInt(counts[1], 10),
      files: parseInt(counts[2], 10),
      note,
    };
  }
  // "no matches found (…)" — zero matches; the engine marker still applies.
  return { engine: engine[1] as "index" | "walk", matches: 0, files: null, note };
}

/**
 * Is `toolName` one of the browser tools? TRUE for BOTH families: the
 * live-tab `browser_*` tools and the headless `offscreen_browser_*` tools
 * (navigate/click/type/eval/screenshot/snapshot). Shared by the ToolCard
 * label/result helpers and by the transcript reducer, which never groups
 * shell or browser calls — every such call renders on its own line
 * (backlog daa38cbe).
 *
 * React-free so the node-env unit tests can exercise it directly.
 */
export function isBrowserToolName(toolName: string): boolean {
  return (
    toolName.startsWith("browser_") ||
    toolName.startsWith("offscreen_browser_")
  );
}

/**
 * One-line label for a browser tool call's args — the TARGET of the call, so
 * the ToolCard header reads `browser_navigate (example.com)` /
 * `browser_click (#submit)` instead of a bare tool name with the meaningful
 * value hidden behind expand (user report 2026-09-09). Covers BOTH families:
 * the live-tab `browser_*` tools and the headless
 * `offscreen_browser_*` tools.
 *
 * - navigate (`browser_navigate` / `offscreen_browser_navigate`) → the `url`
 * - click (`browser_click` / `offscreen_browser_click`) → the `selector`
 * - type (`browser_type` / `offscreen_browser_type`) →
 *   `selector ← "text"` (text truncated to 35 chars with an ellipsis)
 * - eval (`browser_eval` / `offscreen_browser_eval`) → the `expression`
 *   truncated to 45 chars with an ellipsis
 * - screenshot/snapshot have no meaningful arg (the result chip + inline
 *   image carry the information) → null
 *
 * Returns `null` for any other tool name, malformed JSON, or a missing/blank
 * field. React-free so the node-env unit tests can exercise it directly.
 */
export function browserArgLabel(toolName: string, argsJson: string): string | null {
  if (!isBrowserToolName(toolName)) return null;
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(argsJson);
  } catch {
    return null;
  }
  const str = (v: unknown): string | null =>
    typeof v === "string" && v.trim() ? v.trim() : null;
  if (toolName.endsWith("_navigate")) {
    return str(parsed.url);
  }
  if (toolName.endsWith("_click")) {
    return str(parsed.selector);
  }
  if (toolName.endsWith("_type")) {
    const selector = str(parsed.selector);
    if (selector === null) return null;
    const text = str(parsed.text);
    if (text === null) return selector;
    const shown = text.length > 35 ? `${text.slice(0, 35)}…` : text;
    return `${selector} ← "${shown}"`;
  }
  if (toolName.endsWith("_eval")) {
    const expr = str(parsed.expression);
    if (expr === null) return null;
    return expr.length > 45 ? `${expr.slice(0, 45)}…` : expr;
  }
  return null;
}

/**
 * The fetched URL for a `web_fetch` call — the ToolCard header shows it so
 * the card reads `web_fetch (https://example.com)` instead of a bare tool
 * name with the URL hidden behind expand (backlog 69cf7e9c), mirroring the
 * browser family's url chip (`browserArgLabel`). Long URLs are truncated to
 * 60 characters with an ellipsis so a pathological link cannot stretch the
 * header row.
 *
 * Returns `null` for any other tool name, malformed JSON, or a missing/blank
 * `url`. React-free so the node-env unit tests can exercise it directly.
 */
export function webFetchLabel(toolName: string, argsJson: string): string | null {
  if (toolName !== "web_fetch") return null;
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(argsJson);
  } catch {
    return null;
  }
  const url = typeof parsed.url === "string" ? parsed.url.trim() : "";
  if (!url) return null;
  return url.length > 60 ? `${url.slice(0, 60)}…` : url;
}

/**
 * The FULL fetched URL of a `web_fetch` call — unlike {@link webFetchLabel}
 * (the truncated display text) this returns the complete untruncated URL, so
 * a click opens the exact page that was fetched even when the header chip
 * shows only its first 60 characters. Drives the ToolCard header's clickable
 * URL chip (Message.tsx → `openExternal`).
 *
 * Only http(s) URLs qualify: the value is parsed with `new URL` and anything
 * else (`file://`, `javascript:`, a relative path, …) returns `null` so the
 * chip falls back to plain text — an LLM-controlled arg can never turn the
 * chip into a launcher for a non-web target. The returned URL is the parsed
 * URL's normalized `href` (not the raw arg), so the tooltip and the opener
 * input show exactly what a browser would resolve.
 *
 * Returns `null` for any other tool name, malformed JSON, or a missing/blank
 * `url`. React-free so the node-env unit tests can exercise it directly.
 */
export function webFetchUrl(toolName: string, argsJson: string): string | null {
  if (toolName !== "web_fetch") return null;
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(argsJson);
  } catch {
    return null;
  }
  const url = typeof parsed.url === "string" ? parsed.url.trim() : "";
  if (!url) return null;
  try {
    const parsedUrl = new URL(url);
    return parsedUrl.protocol === "http:" || parsedUrl.protocol === "https:" ? parsedUrl.href : null;
  } catch {
    return null;
  }
}

/** Maximum length (in characters) of a tool-error summary line. */
const ERROR_SUMMARY_MAX = 120;

/**
 * One-line human-readable summary of a failed tool result's output — the
 * first non-empty line, trimmed and truncated to {@link ERROR_SUMMARY_MAX}
 * characters with an ellipsis. Shown in the ToolCard below the header so the
 * user sees what went wrong at a glance, without expanding the card or
 * scrolling the raw output. Returns an empty string for empty/whitespace-only
 * input (the caller hides the summary line when empty).
 *
 * The raw tool-result `output` (as carried by the `tool_result` event) is
 * passed verbatim — the `[tool error]` prefix is only added to the message
 * content fed back to the LLM, never to the event payload.
 *
 * React-free so the node-env unit tests can exercise it directly.
 */
export function toolErrorSummary(output: string): string {
  const firstLine = output
    .split(/\r?\n/)
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  if (firstLine === undefined) return "";
  if (firstLine.length <= ERROR_SUMMARY_MAX) return firstLine;
  return firstLine.slice(0, ERROR_SUMMARY_MAX - 1).trimEnd() + "…";
}

/**
 * One-line RESULT summary for a browser tool call — WHAT happened, as a
 * trailing ToolCard chip (user report 2026-09-09: "browser_navigate
 * https://example.com … should show what it does"). Navigate shows the landed
 * URL, snapshot the page title, screenshot the PNG filename; click/type/eval
 * have no meaningful result line beyond the header's arg label → null.
 *
 * Covers BOTH families: the live-tab `browser_*` tools and the headless
 * `offscreen_browser_*` tools (whose navigate result reads
 * "opened page <id> (<title>): <url>"). Returns null for any other tool
 * name, failed results, or unparseable output.
 *
 * React-free so the node-env unit tests can exercise it directly.
 */
export function browserResultInfo(toolName: string, output: string): string | null {
  if (!isBrowserToolName(toolName)) return null;
  if (toolName.endsWith("_navigate")) {
    // browser_navigate: "navigated the Browser tab to <url>".
    const m = /navigated the Browser tab to (\S+)/.exec(output);
    if (m) return `→ ${m[1]}`;
    // offscreen_browser_navigate: "opened page <id> (<title>): <url>".
    const o = /opened page \S+ \([^)]*\): (\S+)/.exec(output);
    return o ? `→ ${o[1]}` : null;
  }
  if (toolName.endsWith("_snapshot")) {
    const t = /<title>([^<]*)<\/title>/.exec(output);
    return t && t[1].trim() ? t[1].trim() : null;
  }
  if (toolName.endsWith("_screenshot")) {
    const p = /\.coding\/browser\/screenshots\/([\w.-]+\.png)/.exec(output);
    return p ? p[1] : null;
  }
  return null;
}

/**
 * The shell tool's call args, parsed: the command plus its optional
 * purpose/cwd annotations. Returns null when the args are not a valid
 * shell-args object (unparseable JSON, wrong shape, missing/empty
 * `command`) — callers fall back to the generic pretty-JSON render.
 *
 * Field names mirror `ShellArgs` in `src/tool/agent/shell.rs` (`command`,
 * `cwd`, `purpose`). React-free so the node-env unit tests can exercise it
 * directly.
 */
export function shellCallFromArgs(
  argsJson: string,
): { command: string; purpose?: string; cwd?: string } | null {
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(argsJson);
  } catch {
    return null;
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  const command = typeof parsed.command === "string" ? parsed.command.trim() : "";
  if (!command) return null;
  const str = (v: unknown): string | undefined =>
    typeof v === "string" && v.trim() !== "" ? v.trim() : undefined;
  const purpose = str(parsed.purpose);
  const cwd = str(parsed.cwd);
  return { command, ...(purpose !== undefined && { purpose }), ...(cwd !== undefined && { cwd }) };
}

/**
 * Split a shell tool call's display output into its human-readable parts.
 *
 * The shell tool builds the display output as
 * `stdout + "\n[stderr]\n" + stderr + "\n[exit code: N]"`
 * (`src/tool/agent/shell.rs`), with output-filter notes (grep nudges,
 * redirect notes) riding at the very top of the stdout section. This helper
 * separates those machine-formatted markers so the ToolCard can render the
 * content readably: stdout as the main block, stderr as a labeled section,
 * and the exit code as a chip.
 *
 * Strictness: the exit code is extracted ONLY when the final line matches
 * `[exit code: <int>]` exactly — a command whose own output happens to end
 * with such a line mid-text is left untouched (the shell tool always appends
 * the trailer as the very last line). The stderr marker splits on its FIRST
 * occurrence (the tool emits at most one). Trailing-newline residue left by
 * the trailer extraction is trimmed.
 */
export function parseShellOutput(output: string): {
  stdout: string;
  stderr: string | null;
  exitCode: number | null;
} {
  let rest = output;
  let exitCode: number | null = null;
  const lines = rest.split("\n");
  const last = lines[lines.length - 1] ?? "";
  const trailer = /^\[exit code: (-?\d+)\]\s*$/.exec(last);
  if (trailer) {
    exitCode = parseInt(trailer[1], 10);
    rest = lines
      .slice(0, -1)
      .join("\n")
      .replace(/\r?\n$/, "")
      .replace(/\r$/, "");
  }
  const marker = "\n[stderr]\n";
  const idx = rest.indexOf(marker);
  if (idx === -1) {
    return { stdout: rest, stderr: null, exitCode };
  }
  return { stdout: rest.slice(0, idx), stderr: rest.slice(idx + marker.length), exitCode };
}

/**
 * The unified diff of a successful `file_edit` call, as carried in the tool
 * result's structured `data` payload (`{"diff": "..."}`, computed Rust-side
 * by the file_edit tool — src/tool/agent/file_edit.rs). Drives the expanded
 * call-detail body: instead of the raw `{"old_string": ...}` args JSON and
 * the one-line "edited path" output, the card renders the actual change via
 * `UnifiedDiffView` (Message.tsx).
 *
 * Returns `null` for anything else — a failed edit (the error text is what
 * matters there), a result without the payload (e.g. an older transcript),
 * or a still-running call — so the caller falls back to the generic
 * pretty-args + raw-output rendering.
 *
 * React-free so the node-env unit tests can exercise it directly.
 */
export function fileEditDiff(result: ToolResult | null): string | null {
  if (result === null || !result.success) return null;
  const data = result.data;
  if (data === null || typeof data !== "object") return null;
  const diff = (data as { diff?: unknown }).diff;
  return typeof diff === "string" && diff.trim() !== "" ? diff : null;
}

/** One parsed `read_files` result section: the file's path plus either the
 *  line range actually read or a short note (error / empty range). */
export type ReadFilesSection =
  | { kind: "range"; path: string; first: number; last: number; total: number }
  | { kind: "note"; path: string; note: string };

/**
 * Parse a `read_files` result's per-file section headers — the expanded
 * call-detail body renders this compact list (path + "lines X–Y of Z",
 * clickable to open the file at the line the read started) instead of
 * dumping every file's full numbered content.
 *
 * The Rust tool (src/tool/agent/read_files.rs) joins sections with blank
 * lines, each headed `=== {path} (lines {first}-{last} of {total}) ===`
 * with `(error)` / `(empty range)` variants; a `SYMBOL NUDGE:` prefix line
 * and `... (truncated ...)` tails ride outside the headers and are ignored.
 * Returns `[]` when no section header matches (e.g. a failed call's error
 * output) — the caller falls back to the generic raw-output rendering.
 *
 * React-free so the node-env unit tests can exercise it directly.
 */
export function parseReadFilesSections(output: string): ReadFilesSection[] {
  const sections: ReadFilesSection[] = [];
  const lines = output.split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    const range = /^=== (.+) \(lines (\d+)-(\d+) of (\d+)\) ===$/.exec(line);
    if (range !== null) {
      sections.push({
        kind: "range",
        path: range[1],
        first: parseInt(range[2], 10),
        last: parseInt(range[3], 10),
        total: parseInt(range[4], 10),
      });
      continue;
    }
    const noted = /^=== (.+) \((error|empty range)\) ===$/.exec(line);
    if (noted !== null) {
      let note = noted[2];
      if (noted[2] === "error") {
        // The error text follows the header — surface its first non-empty
        // line (truncated) so the row says what went wrong, no dump.
        for (let j = i + 1; j < lines.length; j += 1) {
          const text = lines[j].trim();
          if (text === "") continue;
          if (text.startsWith("=== ")) break;
          note = text.length > 100 ? `${text.slice(0, 100)}…` : text;
          break;
        }
      }
      sections.push({ kind: "note", path: noted[1], note });
    }
  }
  return sections;
}

/** Max characters of a backlog item's title shown in the ToolCard header chip. */
const BACKLOG_TITLE_MAX = 80;

/** Extract a display label (e.g. a file path or shell purpose) from a tool call's args. */
export function argLabel(args: string, toolName?: string): string | null {
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(args);
  } catch {
    return null;
  }
  // For shell calls, the `purpose` field is a short human-readable label of
  // what the command does (e.g. "running tests"). Show it in the header so
  // the card reads "shell (running tests)" instead of just "shell".
  if (toolName === "shell") {
    if (typeof parsed.purpose === "string" && parsed.purpose.trim()) {
      return parsed.purpose.trim();
    }
    return null;
  }
  // For spawn_agent calls, the `name` field is the background agent's display
  // name (e.g. "rust-core-reviewer"). Show it so the card reads
  // "spawn_agent (rust-core-reviewer)" instead of a bare "spawn_agent".
  if (toolName === "spawn_agent") {
    if (typeof parsed.name === "string" && parsed.name.trim()) {
      return parsed.name.trim();
    }
    return null;
  }
  // For skill_start calls, the `skill` field is the skill name (e.g.
  // "merge_to_main"). Show it so the card reads "skill_start (merge_to_main)"
  // instead of a bare "skill_start" — mirroring the toolbar path's
  // `▶ skill "merge_to_main" start` announcement.
  if (toolName === "skill_start") {
    if (typeof parsed.skill === "string" && parsed.skill.trim()) {
      return parsed.skill.trim();
    }
    return null;
  }
  // For skill_create calls, the `name` field is the skill being AUTHORED (e.g.
  // "deploy_checklist"). Show it so the card reads
  // "skill_create (deploy_checklist)" instead of a bare "skill_create" —
  // mirroring the skill_start branch above and the toolbar's
  // `▶ skill "…" start` announcement. (skill_reload takes no arguments, so its
  // card stays bare by design.)
  if (toolName === "skill_create") {
    if (typeof parsed.name === "string" && parsed.name.trim()) {
      return parsed.name.trim();
    }
    return null;
  }
  // For git calls, the `subcommand` field is the git verb (e.g. "status",
  // "commit", "merge"). Show it (plus any forwarded read-query `args`) so the
  // card reads "git (log --stat -5)" instead of a bare "git" — the
  // subcommand is the meaningful part of a git call. The git tool's forgiving
  // API also accepts the verb in the `action` field (e.g. action="commit") or
  // a branch/stash action name in `subcommand` (e.g. subcommand="delete"), so
  // fall back to `action` when `subcommand` is absent/blank.
  if (toolName === "git") {
    const subField =
      typeof parsed.subcommand === "string" && parsed.subcommand.trim()
        ? parsed.subcommand.trim()
        : "";
    const actionField =
      typeof parsed.action === "string" && parsed.action.trim() ? parsed.action.trim() : "";
    const sub = subField || actionField;
    if (sub) {
      // The restore subcommand gets a richer label: target/source/paths are
      // the meaningful parts ("restore --staged" unstages, "restore HEAD~1"
      // overwrites from a ref) — a bare "restore" would hide what it does.
      if (sub === "restore") {
        const targetRaw =
          typeof parsed.target === "string" ? parsed.target.trim().toLowerCase() : "";
        // "worktree" is the default — only surface a non-default target.
        const target = targetRaw && targetRaw !== "worktree" ? targetRaw : "";
        const source =
          typeof parsed.source === "string" && parsed.source.trim() ? parsed.source.trim() : "";
        // The backend forgivingly accepts a singular string `path` lifted
        // into `paths` — but ONLY when `paths` is absent or an EMPTY array
        // (a non-empty array wins and `path` is ignored). Mirror that: the
        // array branch applies only to a non-empty array, so a chip for
        // {paths: [], path: "b.txt"} still shows the file the backend will
        // actually restore.
        const pathArray: unknown[] = Array.isArray(parsed.paths) ? parsed.paths : [];
        const pathList =
          pathArray.length > 0
            ? pathArray
                .filter((p): p is string => typeof p === "string" && p.trim().length > 0)
                .join(" ")
            : typeof parsed.path === "string" && parsed.path.trim().length > 0
              ? parsed.path.trim()
              : "";
        const base = [sub, target, source].filter(Boolean).join(" ");
        return pathList ? `${base} -- ${pathList}` : base;
      }
      const extra = Array.isArray(parsed.args)
        ? parsed.args
            .filter((a): a is string => typeof a === "string" && a.trim().length > 0)
            .join(" ")
            .trim()
        : "";
      return extra ? `${sub} ${extra}` : sub;
    }
    return null;
  }
  // For git_read calls, the `op` field (diff | log | show) is the meaningful
  // part — show it (plus the commit for show, the path filter for log/diff,
  // and log's limit) so the card reads "git_read (log -8)" or
  // "git_read (show d31b606)" instead of a bare "git_read" with no
  // information (user report 2026-08-25: "git read should show some
  // information and stop looking like it is in progress when done").
  if (toolName === "git_read") {
    const op = typeof parsed.op === "string" && parsed.op.trim() ? parsed.op.trim() : "";
    if (!op) return null;
    const arg = typeof parsed.commit === "string" && parsed.commit.trim() ? parsed.commit.trim() : "";
    const path = typeof parsed.path === "string" && parsed.path.trim() ? parsed.path.trim() : "";
    if (op === "show" && arg) {
      // Short hash when long — keeps the chip compact.
      return `show ${arg.length > 7 ? arg.slice(0, 7) : arg}`;
    }
    if (op === "log") {
      const limit = typeof parsed.limit === "number" && Number.isFinite(parsed.limit) ? ` -${parsed.limit}` : "";
      const filter = path ? ` -- ${path}` : "";
      return `log${limit}${filter}`.trim();
    }
    if (path) {
      return `${op} ${path}`;
    }
    return op;
  }
  // For read_files calls, the `files` field is an array of read specs, each
  // with a `path`. Show the basenames so the card reads
  // "read_files (mod.rs, agent.rs)" instead of a bare "read_files". The Rust
  // tool also accepts a single-file top-level `path` shorthand (absorbed from
  // the old file_read tool); when `files` is absent/not-an-array, fall through
  // to the common path/file fallback below so that shape still shows the name.
  if (toolName === "read_files") {
    const files = parsed.files;
    if (Array.isArray(files)) {
      const names = files
        .map((f) => (f && typeof f === "object" && typeof (f as Record<string, unknown>).path === "string" ? (f as Record<string, string>).path : null))
        .filter((p): p is string => !!p)
        .map((p) => basename(p));
      if (names.length === 0) return null;
      const cap = 3;
      const shown = names.slice(0, cap).join(", ");
      const extra = names.length > cap ? ` +${names.length - cap}` : "";
      return `${shown}${extra}`;
    }
  }
  // For search / search_read calls, the `pattern` field is the meaningful
  // part (what's being searched for). Show it (quoted) plus an optional glob
  // so the card reads "search (\"WorkflowState\")" or
  // "search (\"WorkflowState\" in **/*.rs)" instead of a bare "search".
  if (toolName === "search" || toolName === "search_read") {
    const pattern = typeof parsed.pattern === "string" ? parsed.pattern.trim() : "";
    if (!pattern) return null;
    const glob = typeof parsed.glob === "string" && parsed.glob.trim() ? parsed.glob.trim() : "";
    return glob ? `"${pattern}" in ${glob}` : `"${pattern}"`;
  }
  // For backlog_add calls, the `text` field is the backlog item's title — the
  // task/prompt being queued. Show its first line (truncated + quoted) so the
  // card reads `backlog_add "Fix the stop_token_ids…"` instead of a bare
  // `backlog_add` with the title hidden behind expand (backlog fef458f1).
  // Mirrors search's quoted free-text pattern chip; only the first line is
  // used so a multi-line prompt collapses to a one-line header title.
  if (toolName === "backlog_add") {
    const text = typeof parsed.text === "string" ? parsed.text : "";
    const firstLine = text.split(/\r?\n/)[0].trim();
    if (!firstLine) return null;
    const shown =
      firstLine.length > BACKLOG_TITLE_MAX
        ? firstLine.slice(0, BACKLOG_TITLE_MAX - 1).trimEnd() + "…"
        : firstLine;
    return `"${shown}"`;
  }
  // For the graph_* tools, the meaningful part is WHAT was looked up in the
  // code graph: the search query, the symbol name/id, or the from→to
  // endpoints. Show it so the card reads `graph_search ("GraphView")`
  // instead of a bare `graph_search` — mirroring search's pattern chip.
  const graph = graphCallLabel(toolName ?? "", args);
  if (graph !== null) {
    return graph;
  }
  // For the browser family (live-tab browser_* + headless
  // offscreen_browser_*), the meaningful part is the TARGET: the URL to
  // navigate, the selector to click/type, or the JS expression to eval —
  // so the card reads "browser_navigate (example.com)" instead of a bare
  // tool name with the URL hidden behind expand (user report 2026-09-09).
  const browser = browserArgLabel(toolName ?? "", args);
  if (browser !== null) {
    return browser;
  }
  // For web_fetch, the `url` field is the meaningful part — show it so the
  // card reads "web_fetch (https://example.com)" instead of a bare
  // "web_fetch" with the URL hidden behind expand (backlog 69cf7e9c).
  const webFetch = webFetchLabel(toolName ?? "", args);
  if (webFetch !== null) {
    return webFetch;
  }
  // Common path-bearing fields across our tools.
  const path =
    (typeof parsed.path === "string" && parsed.path) ||
    (typeof parsed.file === "string" && parsed.file) ||
    null;
  if (path) {
    // Show just the file name, not the full path.
    return basename(path);
  }
  return null;
}

/** Map a raw tool name to a friendlier display label for the ToolCard header
 *  (e.g. "spawn_agent" → "Spawn Agent"). Falls back to the raw name. The
 *  existing `(name)` parenthetical from `argLabel` is appended separately. */
export function displayName(name: string): string {
  switch (name) {
    case "spawn_agent":
      return "Spawn Agent";
    case "graph_search":
      return "Graph Search";
    case "graph_context":
      return "Graph Context";
    case "graph_impact":
      return "Graph Impact";
    case "graph_path":
      return "Graph Path";
    default:
      return name;
  }
}

/**
 * The visible preview of a running call's live output tail (backlog 7e6385b3):
 * the LAST `maxLines` lines, further capped to `maxChars` counted from the end
 * (newest-first trimming), so a chatty command's newest lines stay on screen
 * without re-rendering kilobytes per chunk.
 *
 * The reducer retains far more (16 KiB per running call, head-dropped) — that
 * window is what makes a scroll-back possible, while this only bounds the DOM
 * node. A trailing newline is preserved, so a partially printed line still
 * reads as one.
 */
export function liveTailPreview(text: string, maxLines = 6, maxChars = 400): string {
  const lines = text.split("\n");
  const tail = lines.length > maxLines ? lines.slice(-maxLines) : lines;
  const joined = tail.join("\n");
  return joined.length > maxChars ? joined.slice(-maxChars) : joined;
}
