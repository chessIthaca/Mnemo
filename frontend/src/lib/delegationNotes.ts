// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Display-layer filter for the steering notes that ride in tool output.
 *
 * Steering notes are MODEL guidance injected into a tool result — the
 * AUTO-DELEGATED block, the shell/literal TIPs, the graph-miss hint, the
 * RECALLED CONTEXT rider, the read nudge, the known-memory-hit note, the
 * consolidation reminder, the stale-read note. Each one is emitted by the
 * backend (`MarkerKind::marker()` / `MarkerKind::detect()` in
 * src/agent/steering_stats.rs — the matchers below MIRROR those emission
 * sites, in prose: keeping them in lockstep is a review duty, the same
 * cross-root reasoning as the windowRestore constants).
 *
 * The `[ui.steering_notes]` config table (Settings → Chat) hides any subset
 * of them from the CHAT DISPLAY ONLY — the contract the original
 * single-toggle filter had for AUTO-DELEGATED (DECISION 950fdba5): the tool
 * result TEXT — the model's context, including a note's re-issue escape
 * hatch — is never modified.
 *
 * MATCHING — two precise modes, never a bare substring scan of the line:
 *
 * - `linePrefixes` are tested against the note-UNWRAPPED text (an optional
 *   `note: ` / `NOTE: ` wrapper is stripped first) and must match at the
 *   START. This covers the kinds whose line begins with their marker.
 * - `linePatterns` are tested against the RAW line and encode the shapes
 *   that do not lead a line: the graph miss is a JSON field value
 *   (`"hint": "No symbols matched …"`), the stale-read note rides the TAIL
 *   of the hybrid `file_edit` error line, and the advisory symbol nudge is
 *   wrapped. Each pattern is anchored to a line start or scoped to its own
 *   marker shape, so a note QUOTED inside a result line stays visible where
 *   that line carries a location prefix (search/read results prefix every hit
 *   with `path:line:`) — the ONE exception being `edit_stale_read`, whose
 *   unanchored drift-sentence match fires wherever the sentence sits, prefix
 *   included. Otherwise only an unfiltered raw stream — the shell
 *   stdout/stderr and the generic `<pre>` fallback — can match a note copied
 *   verbatim at column 0. The backend's own detection is first-line scoped
 *   wherever a result could echo foreign content (`steering_stats.rs`
 *   `detect()` — the graph-miss, recall-rider and consolidation notes scan the
 *   whole output instead: the first two sit inside a tool gate that cannot
 *   carry another kind, the third is appended by the dispatcher to any
 *   successful result and kept unambiguous by its dispatcher-specific
 *   phrasing), and its tests pin that a grep result quoting the shell TIP does
 *   not fire.
 *
 * MERGED NOTES — `merged_note` / `push_note` (search.rs) join several notes
 * onto ONE `note: ` line with `"; "`, and the modules' own notes contain
 * `"; "` internally (the grep TIP, the known-memory-hit note). So a line is
 * split into components only where a `"; "` is FOLLOWED BY a recognized note
 * start; each component is then filtered on its own, which keeps one toggle
 * from dragging an unrelated note's text out of the display and keeps the
 * unhidden remainder byte-identical.
 *
 * React-free so the node-env unit tests can exercise it directly.
 */

/**
 * The stable snake_case key of a steering-note kind — mirrors
 * `UiConfig::STEERING_NOTE_KEYS` in src/config/general.rs (and the
 * `MarkerKind::label()` set, plus the `auto_delegated` display family).
 */
export type SteeringNoteKey =
  | "auto_delegated"
  | "search_nudge"
  | "shell_tip"
  | "graph_miss"
  | "recall_rider"
  | "read_nudge"
  | "literal_tip"
  | "known_memory_hit"
  | "consolidation_due"
  | "shell_redirect"
  | "edit_stale_read";

/** One steering-note kind: what it is, what it looks like, how it defaults. */
export interface SteeringNoteDef {
  /** The config key (`[ui.steering_notes]` field). */
  key: SteeringNoteKey;
  /** The backend marker (`MarkerKind::marker()`) — for docs/telemetry parity. */
  marker: string;
  /**
   * Line-leading prefixes, matched against the note-unwrapped text (an
   * optional `note: ` / `NOTE: ` wrapper is stripped first).
   */
  linePrefixes?: readonly string[];
  /**
   * Precise patterns for shapes that do not lead a line, matched against the
   * RAW line (a `note: ` wrapper is part of the subject where the emitter
   * adds one — see each entry).
   */
  linePatterns?: readonly RegExp[];
  /** Chat-settings label. */
  label: string;
  /** Chat-settings hint (what the toggle does). */
  description: string;
  /** Whether the note is displayed when the config has no explicit override. */
  defaultVisible: boolean;
}

/**
 * The steering-note registry, in Chat-settings order.
 *
 * `auto_delegated` is the display FAMILY that the legacy
 * `[ui].show_delegation_notes` toggle has always covered (the code-graph
 * delegation and its memory twin — `AUTO-DELEGATED to memory` carries no
 * marker substring of its own), and it is the only kind hidden by default:
 * an untouched config therefore displays exactly what it displayed before
 * the per-kind table existed.
 *
 * NOTE the deliberate overlap: the code-graph delegation header
 * ("AUTO-DELEGATED to the code graph — 'X' is an indexed symbol …") IS the
 * `search_nudge` emission too — the backend counts it as both (the
 * delegation fulfils the nudge in place), so it is hidden when EITHER kind is
 * disabled. The toggles union; they never fight.
 */
export const STEERING_NOTES: readonly SteeringNoteDef[] = [
  {
    key: "auto_delegated",
    marker: "AUTO-DELEGATED",
    // Both emission shapes: the raw fast-path block header (the `search`
    // output STARTS with it — search.rs) and the `note: `-wrapped merge
    // (merged_note's first component; the wrapper is stripped before
    // matching).
    linePrefixes: ["AUTO-DELEGATED"],
    label: "Auto-delegation notes (search → graph/memory)",
    description:
      "The AUTO-DELEGATED header search/search_read prepend when a query is answered by the code graph or memory. The delegated answer itself always stays visible.",
    defaultVisible: false,
  },
  {
    key: "search_nudge",
    marker: "is an indexed symbol",
    // Four real shapes: (1) the auto-delegated header — the delegation IS
    // the nudge, fulfilled in-place (steering_stats `detect` counts it and
    // registers the switch); (2)+(3) the two advisory forms `symbol_nudge`
    // emits on the escape repeat / the fuzzy branch, which the tool wraps as
    // `note: …` (search.rs, wrapped by `with_note`); (4) the alternation
    // absorption note (`alternation_nudge`, search.rs) — `from the
    // alternation pattern '{pattern}': …` with one `; `-joined entry per
    // resolved branch, every entry ending in "is an indexed symbol". The
    // entry whose name equals its branch is emitted UNQUOTED, so it is the
    // note's own leading phrase that identifies this shape (not the
    // per-entry quoting).
    linePrefixes: ["AUTO-DELEGATED", "from the alternation pattern"],
    linePatterns: [
      /^(?:note: |NOTE: )?'[^']*' is an indexed symbol/,
      /^(?:note: |NOTE: )?the symbol '[^']*' \(from pattern '[^']*'\) is an indexed symbol/,
    ],
    label: "Search symbol nudge",
    description:
      "The “is an indexed symbol” nudge steering a symbol-shaped search to the graph tools — it rides the auto-delegated header, or the advisory note when the search is re-issued to escape delegation.",
    defaultVisible: true,
  },
  {
    key: "shell_tip",
    marker: "TIP: for file-content search",
    // GREP_NUDGE, inserted at position 0 of a grep-family command's result
    // (shell.rs) — so it leads the stdout the card renders. The note
    // contains "; " internally; the component splitter only breaks on a
    // recognized note start, so it stays one component.
    linePrefixes: ["TIP: for file-content search"],
    label: "Shell content-search TIP",
    description:
      "The TIP a `shell` grep-family command gets: use the content-indexed search tool instead.",
    defaultVisible: true,
  },
  {
    key: "graph_miss",
    marker: "No symbols matched",
    // `miss_hint` (codegraph.rs) is a JSON FIELD VALUE, and the payload is
    // serialized with `to_string_pretty` — the rendered line is
    // `  "hint": "No symbols matched 'x'. …"`, never marker-led. (The other
    // arm, "no exact symbol; try one of the candidates' ids", is a different
    // hint and NOT this marker.)
    linePatterns: [/^\s*"hint":\s*"No symbols matched/],
    label: "Graph miss hint",
    description:
      "The hint shown when a graph lookup matches no symbols.",
    defaultVisible: true,
  },
  {
    key: "recall_rider",
    marker: "RECALLED CONTEXT",
    // The passive recall block's header (steering.rs builds a
    // `RECALLED CONTEXT (…)` line), appended to create_plan results and
    // spawned-agent tasks.
    linePrefixes: ["RECALLED CONTEXT"],
    label: "Recalled-context rider",
    description:
      "The prior-knowledge rider appended to create_plan results and spawned-agent tasks.",
    defaultVisible: true,
  },
  {
    key: "read_nudge",
    marker: "SYMBOL NUDGE:",
    // read_files prepends `SYMBOL NUDGE: …` as the single leading line
    // (read_files.rs). Note: the expanded read card normally renders the
    // parsed per-file row list, which does not include that line — the
    // toggle governs the raw-output render (a failed or header-less read).
    linePrefixes: ["SYMBOL NUDGE:"],
    label: "Read symbol nudge",
    description:
      "The nudge prepended to a whole-file read of a large indexed source file, pointing at graph_context. (The compact read card shows the file list without it.)",
    defaultVisible: true,
  },
  {
    key: "literal_tip",
    marker: "TIP: pattern has no regex metacharacters",
    // Rides the merged note as its own "; "-joined component (search.rs
    // `literal_tip` + `merged_note`), wrapped once as `note: …`.
    linePrefixes: ["TIP: pattern has no regex metacharacters"],
    label: "Literal-engine TIP",
    description:
      "The TIP that a metacharacter-free search pattern could use `literal: true` for the indexed engine.",
    defaultVisible: true,
  },
  {
    key: "known_memory_hit",
    marker: "known memory hit:",
    // Same merged note, its own component (search.rs). The component itself
    // contains "; " ("… backlog item; memory_search it for detail") — hence
    // the recognized-start rule in the splitter.
    linePrefixes: ["known memory hit:"],
    label: "Known-memory-hit note",
    description:
      "The note shown when a search pattern matches a known backlog id.",
    defaultVisible: true,
  },
  {
    key: "consolidation_due",
    marker: "working-memory events accumulated this session",
    // Dispatcher-appended (steering.rs): `NOTE: {n} working-memory events
    // accumulated this session — run memory_consolidate(session_id) to
    // distill them`. The event count is a digit run, so this kind is pinned
    // by an anchored pattern rather than a prefix.
    linePatterns: [/^NOTE: \d+ working-memory events accumulated this session/],
    label: "Consolidation reminder",
    description:
      "The reminder that working-memory events accumulated and a consolidation is due.",
    defaultVisible: true,
  },
  {
    key: "shell_redirect",
    marker: "TIP: output redirection detected",
    // REDIRECT_NOTE, prepended like GREP_NUDGE (shell.rs) — note its own
    // internal "; ".
    linePrefixes: ["TIP: output redirection detected"],
    label: "Shell-redirect TIP",
    description:
      "The TIP shown when a shell command redirects output to a null sink, hiding failure details.",
    defaultVisible: true,
  },
  {
    key: "edit_stale_read",
    marker: "Re-read the file",
    // NOT line-led: `with_fresh_read_nudge` (file_edit.rs) appends the
    // marker mid-line to the drift-class error text, which IS the result
    // output's first line ("… — the content has likely drifted from your last
    // read. Re-read the file (read_files, this exact path) and retry …").
    // The pattern requires the drift clause AND the marker in order, so a
    // file's content quoting "Re-read the file" is never hidden.
    linePatterns: [/— the content has likely drifted from your last read\.\s*Re-read the file/],
    label: "Stale-read note",
    description:
      "The note after a failed file_edit: the file changed, re-read it before retrying.",
    defaultVisible: true,
  },
];

/** Fast lookup by key (module-private; the registry is the source of truth). */
const NOTE_BY_KEY: Record<string, SteeringNoteDef> = Object.fromEntries(
  STEERING_NOTES.map((def) => [def.key, def]),
);

/**
 * The kinds hidden when the config carries no override — what the legacy
 * `show_delegation_notes` default meant.
 */
export const DEFAULT_HIDDEN_STEERING_NOTES: readonly SteeringNoteKey[] =
  STEERING_NOTES.filter((def) => !def.defaultVisible).map((def) => def.key);

/** The `note: ` / `NOTE: ` wrappers a steering line may carry. */
const NOTE_PREFIXES = ["note: ", "NOTE: "] as const;

/** Split a line into its optional wrapper and the note text. */
function unwrapNote(line: string): { wrapper: string; text: string } {
  for (const prefix of NOTE_PREFIXES) {
    if (line.startsWith(prefix)) return { wrapper: prefix, text: line.slice(prefix.length) };
  }
  return { wrapper: "", text: line };
}

/** Whether `text` matches `def` (prefixes on unwrapped text, patterns on raw). */
function matchesDef(text: string, rawLine: string, def: SteeringNoteDef): boolean {
  if (def.linePatterns?.some((rx) => rx.test(rawLine))) return true;
  return (def.linePrefixes ?? []).some((prefix) => text.startsWith(prefix));
}

/** Whether `text` begins a RECOGNIZED steering-note component. */
function isComponentStart(text: string): boolean {
  return STEERING_NOTES.some((def) => matchesDef(text, text, def));
}

/** The offsets where a `"; "` genuinely starts a new note component. */
function componentStarts(text: string): number[] {
  const starts = [0];
  let idx = text.indexOf("; ");
  while (idx !== -1) {
    if (isComponentStart(text.slice(idx + 2))) starts.push(idx + 2);
    idx = text.indexOf("; ", idx + 2);
  }
  return starts;
}

/**
 * Remove every hidden kind from one line of tool output. Returns the (possibly
 * rewritten) line, or `null` when nothing displayable remains.
 *
 * Whole-line matches go first (the JSON hint, the hybrid stale-read error
 * line); then a single-component line matches or not; then a MERGED note line
 * is split at its recognized component starts and only the hidden components
 * are dropped — the visible remainder is re-joined byte-identically.
 */
function filterLine(line: string, defs: readonly SteeringNoteDef[]): string | null {
  if (line === "") return line;
  if (defs.some((def) => matchesDef("", line, def))) return null;
  const { wrapper, text } = unwrapNote(line);
  if (text === "") return line;
  const starts = componentStarts(text);
  if (starts.length === 1) {
    return defs.some((def) => matchesDef(text, text, def)) ? null : line;
  }
  const parts: string[] = [];
  for (let i = 0; i < starts.length; i += 1) {
    const from = starts[i];
    const to = i + 1 < starts.length ? starts[i + 1] - 2 : text.length;
    parts.push(text.slice(from, to));
  }
  const visible = parts.filter((part) => !defs.some((def) => matchesDef(part, part, def)));
  if (visible.length === 0) return null;
  if (visible.length === parts.length) return line;
  return `${wrapper}${visible.join("; ")}`;
}

/** The hidden kinds, resolved to registry entries (unknown keys are ignored). */
function hiddenDefs(hidden: readonly string[]): SteeringNoteDef[] {
  return hidden
    .map((key) => NOTE_BY_KEY[key])
    .filter((def): def is SteeringNoteDef => def !== undefined);
}

/**
 * Filter ONE note text (a tool-output line, or a note extracted by
 * `searchResultInfo` — the text WITHOUT its `note: ` wrapper): returns the
 * visible remainder, or `null` when a hidden kind hid everything.
 *
 * The ToolCard's collapsed note chip uses this so a merged note whose hidden
 * component is dropped still shows the rest.
 */
export function filterSteeringNote(
  text: string,
  hidden: readonly string[],
): string | null {
  const defs = hiddenDefs(hidden);
  if (defs.length === 0) return text;
  return filterLine(text, defs);
}

/** Whether a note is hidden ENTIRELY by the given hidden kinds. */
export function isSteeringNoteHidden(text: string, hidden: readonly string[]): boolean {
  return filterSteeringNote(text, hidden) === null;
}

/**
 * Remove every hidden kind's note from a tool result. Only note text goes —
 * the underlying tool output, the delegated answer (def:/callers: lines), a
 * non-hidden component of the SAME line, and every unrelated line stay
 * byte-identical.
 */
export function stripSteeringNotes(output: string, hidden: readonly string[]): string {
  const defs = hiddenDefs(hidden);
  if (defs.length === 0) return output;
  return output
    .split("\n")
    .map((line) => filterLine(line, defs))
    .filter((line): line is string => line !== null)
    .join("\n");
}

/**
 * The kinds to hide, given the resolved `[ui.steering_notes]` object from the
 * backend (one boolean per kind — see `GetSettingsSteeringNotes`).
 *
 * An ABSENT object means a backend that predates the per-kind table (partial
 * upgrade / stale binary), so the legacy single toggle keeps governing the
 * family it always covered: `legacyShowDelegationNotes === true` hides
 * nothing, otherwise {@link DEFAULT_HIDDEN_STEERING_NOTES}. Passing it is not
 * optional politeness — without it, a config carrying
 * `[ui] show_delegation_notes = true` would silently lose that preference at
 * hydration, and the next Chat save would persist `auto_delegated: false` and
 * make the loss permanent.
 */
export function hiddenKeysFromConfig(
  cfg?: Partial<Record<SteeringNoteKey, boolean>> | null,
  legacyShowDelegationNotes?: boolean | null,
): SteeringNoteKey[] {
  if (!cfg) {
    return legacyShowDelegationNotes ? [] : [...DEFAULT_HIDDEN_STEERING_NOTES];
  }
  return STEERING_NOTES.filter((def) => cfg[def.key] === false).map((def) => def.key);
}

/**
 * Whether a note (as extracted by `searchResultInfo` — the note text WITHOUT
 * the `note: ` prefix) is an AUTO-DELEGATED note.
 *
 * TEST-ONLY compatibility shim: no production caller since the per-kind
 * filter landed (`Message.tsx` uses {@link filterSteeringNote}), kept to pin
 * the legacy single-toggle behavior this feature replaced.
 */
export function isDelegationNote(note: string): boolean {
  return note.startsWith("AUTO-DELEGATED");
}

/**
 * Remove the AUTO-DELEGATED steering text from a tool result — both emission
 * shapes (raw fast-path block header and `note: `-prefixed prepend).
 *
 * TEST-ONLY compatibility shim: no production caller since the per-kind
 * filter landed, kept to pin the legacy single-toggle behavior (the default
 * render must stay equivalent to the old prefix list).
 */
export function stripDelegationNotes(text: string): string {
  return stripSteeringNotes(text, ["auto_delegated"]);
}
