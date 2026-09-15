// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * delegationNotes — tests for the display-layer filter that hides steering
 * notes from tool output, per kind (Chat settings → `[ui.steering_notes]`,
 * one toggle per note; only auto-delegation is hidden by default — the
 * behavior the original single `[ui].show_delegation_notes` toggle had).
 *
 * Pure-helper tests plus static source-contract pins on the ToolCard render
 * sites (vitest runs in `node` env — no React DOM infra), in the style of
 * ChatSection.test.ts.
 */

import { describe, expect, it } from "vitest";
import {
  DEFAULT_HIDDEN_STEERING_NOTES,
  STEERING_NOTES,
  hiddenKeysFromConfig,
  isDelegationNote,
  isSteeringNoteHidden,
  stripDelegationNotes,
  stripSteeringNotes,
  type SteeringNoteKey,
} from "./delegationNotes";
import messageSource from "../components/chat/Message.tsx?raw";

/**
 * One representative output LINE per kind — the real emission shapes.
 * `graph_miss` and `edit_stale_read` are VERBATIM from their emission sites
 * (`codegraph.rs` `miss_hint` inside the pretty-JSON `"hint"` field;
 * `file_edit.rs` `with_fresh_read_nudge` appended to the drift error) — the
 * matchers in the registry mirror those sites, so keep them in lockstep.
 */
const LINES: Record<SteeringNoteKey, string> = {
  auto_delegated:
    "note: AUTO-DELEGATED to the code graph — 'ChatDraft' is an indexed symbol (re-issue this exact search to get the plain file search instead):",
  search_nudge:
    "AUTO-DELEGATED to the code graph — 'hello' is an indexed symbol (re-issue this exact search to get the plain file search instead):",
  shell_tip:
    "TIP: for file-content search use the `search` tool (sandboxed, content-indexed)",
  graph_miss:
    "  \"hint\": \"No symbols matched 'zzz'. The graph indexes symbol definitions only — string literals (tool names, config keys, log text) are not indexed; use `search` for those. For a partial symbol name, retry with a shorter query.\"",
  recall_rider: "RECALLED CONTEXT (2 hit(s)) — prior knowledge",
  read_nudge: "SYMBOL NUDGE: this file defines `Foo` — graph_context shows its callers",
  literal_tip:
    "note: TIP: pattern has no regex metacharacters — literal:true would use the content-index engine (one indexed lookup instead of a tree walk)",
  known_memory_hit: "note: known memory hit: backlog 1a5bffcf — 'sandbox bypass'",
  consolidation_due:
    "NOTE: 15 working-memory events accumulated this session — consider memory_consolidate",
  shell_redirect:
    "TIP: output redirection detected — results are in out.txt, read it with read_files",
  edit_stale_read:
    "old_string not found in file — the content has likely drifted from your last read. Re-read the file (read_files, this exact path) and retry with the exact current text; do not edit without a fresh read.",
};

/**
 * `search_nudge` and `auto_delegated` deliberately share the delegation
 * header line (the marker IS that header) — hiding either hides it.
 */
const TWIN: Record<string, SteeringNoteKey> = {
  auto_delegated: "search_nudge",
  search_nudge: "auto_delegated",
};

const ALL_KEYS = STEERING_NOTES.map((def) => def.key);

describe("the steering-note registry", () => {
  it("lists every kind exactly once (the config keys, in Chat order)", () => {
    expect(ALL_KEYS).toEqual([
      "auto_delegated",
      "search_nudge",
      "shell_tip",
      "graph_miss",
      "recall_rider",
      "read_nudge",
      "literal_tip",
      "known_memory_hit",
      "consolidation_due",
      "shell_redirect",
      "edit_stale_read",
    ]);
    expect(new Set(ALL_KEYS).size).toBe(ALL_KEYS.length);
  });

  it("carries a marker, label, description and a matcher for every kind", () => {
    for (const def of STEERING_NOTES) {
      expect(def.marker, def.key).not.toBe("");
      expect(def.label, def.key).not.toBe("");
      expect(def.description, def.key).not.toBe("");
      const matchers =
        (def.linePrefixes?.length ?? 0) + (def.linePatterns?.length ?? 0);
      expect(matchers, def.key).toBeGreaterThan(0);
    }
  });

  it("hides only the auto-delegation family by default", () => {
    expect(DEFAULT_HIDDEN_STEERING_NOTES).toEqual(["auto_delegated"]);
  });

  it("mirrors the MarkerKind markers it filters on", () => {
    const byKey = Object.fromEntries(STEERING_NOTES.map((d) => [d.key, d.marker]));
    expect(byKey.search_nudge).toBe("is an indexed symbol");
    expect(byKey.shell_tip).toBe("TIP: for file-content search");
    expect(byKey.graph_miss).toBe("No symbols matched");
    expect(byKey.recall_rider).toBe("RECALLED CONTEXT");
    expect(byKey.read_nudge).toBe("SYMBOL NUDGE:");
    expect(byKey.literal_tip).toBe("TIP: pattern has no regex metacharacters");
    expect(byKey.known_memory_hit).toBe("known memory hit:");
    expect(byKey.consolidation_due).toBe(
      "working-memory events accumulated this session",
    );
    expect(byKey.shell_redirect).toBe("TIP: output redirection detected");
    expect(byKey.edit_stale_read).toBe("Re-read the file");
  });
});

describe("stripSteeringNotes", () => {
  it.each(Object.entries(LINES))(
    "hides the %s line when its own toggle is off",
    (key, line) => {
      expect(stripSteeringNotes(line, [key]).trim()).toBe("");
    },
  );

  it.each(Object.entries(LINES))(
    "keeps the %s line while only the OTHER kinds are hidden",
    (key, line) => {
      const others = ALL_KEYS.filter((k) => k !== key && k !== TWIN[key]);
      expect(stripSteeringNotes(line, others)).toBe(line);
    },
  );

  it("hides the delegation header when EITHER twin is off (documented overlap)", () => {
    expect(stripSteeringNotes(LINES.auto_delegated, ["search_nudge"]).trim()).toBe("");
    expect(stripSteeringNotes(LINES.search_nudge, ["auto_delegated"]).trim()).toBe("");
  });

  it("hides both advisory search-nudge shapes (the escape-repeat notes)", () => {
    // Verbatim from search.rs `symbol_nudge` — the two branches, wrapped as
    // `note: …` by `with_note`; the `AUTO-DELEGATED` prefix alone never
    // matched these, so they ride the registry's linePatterns.
    const note =
      "note: 'hello' is an indexed symbol — graph_context(id=\"src/lib.rs::hello::12\") gives its definition + callers in one call; prefer the graph tools for symbol lookups (search is for text)";
    const fuzzy =
      "note: the symbol 'hello' (from pattern 'hel*') is an indexed symbol — graph_context(id=\"src/lib.rs::hello::12\") gives its definition + callers in one call; prefer the graph tools for symbol lookups (search is for text)";
    expect(stripSteeringNotes(note, ["search_nudge"]).trim()).toBe("");
    expect(stripSteeringNotes(fuzzy, ["search_nudge"]).trim()).toBe("");
  });

  it("hides the alternation-absorption note (closing review LOW 1, verbatim)", () => {
    // Verbatim shape from search.rs `alternation_nudge`, wrapped by
    // `with_note`: the note leads with its OWN phrase, and the entry whose
    // name equals its branch is emitted UNQUOTED — so the phrase, not the
    // per-entry quoting, is what the registry matches.
    const quoted =
      'note: from the alternation pattern \'steering_notes|SteeringNotesPatch\': \'steering_notes\' resolves to \'steering_notes_round_trips\', an indexed symbol — graph_context(id="src/config/general.rs::steering_notes_round_trips::1350"); prefer the graph tools for symbol hunts (search is for text)';
    const unquoted =
      'note: from the alternation pattern \'Strip|stripping\': Strip is an indexed symbol — graph_context(id="a.rs::Strip::1"); prefer the graph tools for symbol hunts (search is for text)';
    expect(stripSteeringNotes(quoted, ["search_nudge"]).trim()).toBe("");
    expect(stripSteeringNotes(unquoted, ["search_nudge"]).trim()).toBe("");
    // The per-entry "; " is not a recognized component start, so hiding
    // every OTHER kind leaves the alternation note byte-identical.
    expect(stripSteeringNotes(quoted, ALL_KEYS.filter((k) => k !== "search_nudge"))).toBe(quoted);
  });

  it("hides only the targeted component of a MERGED note (verbatim pair)", () => {
    // Verbatim co-occurrence pinned by steering_stats.rs
    // (`co_occurring_markers_each_count`): `merged_note` joins the literal
    // TIP and the known-memory note with "; " onto ONE `note: ` line, and the
    // known-memory component carries a "; " of its own — the splitter must
    // break only at recognized note starts, and each toggle drops only its
    // own component (byte-identical remainder).
    const merged =
      "note: TIP: pattern has no regex metacharacters — literal:true would use the content-index engine (one indexed lookup instead of a tree walk); known memory hit: 'PLAN: backlog 70f5b248' — this id is a known backlog item; memory_search it for detail";
    expect(stripSteeringNotes(merged, ["known_memory_hit"])).toBe(
      "note: TIP: pattern has no regex metacharacters — literal:true would use the content-index engine (one indexed lookup instead of a tree walk)",
    );
    expect(stripSteeringNotes(merged, ["literal_tip"])).toBe(
      "note: known memory hit: 'PLAN: backlog 70f5b248' — this id is a known backlog item; memory_search it for detail",
    );
    expect(stripSteeringNotes(merged, ["literal_tip", "known_memory_hit"])).toBe("");
  });

  it("recognizes the alternation note as a component start when merged", () => {
    // Synthetic co-occurrence (an alternation pattern carries metacharacters,
    // so it never rides with the literal TIP): pins that the alternation
    // prefix doubles as `isComponentStart`, so a merged alternation
    // component is droppable on its own without touching its sibling.
    const merged =
      'note: TIP: pattern has no regex metacharacters — literal:true would use the content-index engine (one indexed lookup instead of a tree walk); from the alternation pattern \'Strip|stripping\': Strip is an indexed symbol — graph_context(id="a.rs::Strip::1"); prefer the graph tools for symbol hunts (search is for text)';
    expect(stripSteeringNotes(merged, ["literal_tip"])).toBe(
      'note: from the alternation pattern \'Strip|stripping\': Strip is an indexed symbol — graph_context(id="a.rs::Strip::1"); prefer the graph tools for symbol hunts (search is for text)',
    );
  });

  it("is a byte-identical no-op when nothing is hidden", () => {
    const text = ALL_KEYS.map((k) => LINES[k]).join("\n");
    expect(stripSteeringNotes(text, [])).toBe(text);
  });

  it("keeps the delegated answer and the surrounding tool output", () => {
    const out = stripSteeringNotes(
      [
        LINES.auto_delegated,
        "  def: frontend/src/components/settings/types.ts::ChatDraft::375 (interface, lines 375-384)",
        "  full 360° view: graph_context(id=\"…\")",
        "",
        "1 matches in 1 files (engine: index)",
      ].join("\n"),
      ["auto_delegated"],
    );
    expect(out).toContain("def: frontend/src/components/settings/types.ts::ChatDraft::375");
    expect(out).toContain("full 360° view:");
    expect(out).toContain("1 matches in 1 files (engine: index)");
    expect(out).not.toContain("AUTO-DELEGATED");
  });

  it("removes every hidden kind at once, leaving the visible ones", () => {
    const text = ALL_KEYS.map((k) => LINES[k]).join("\n");
    const out = stripSteeringNotes(text, ["literal_tip", "known_memory_hit", "shell_tip"]);
    expect(out).not.toContain("literal:true would use the content-index engine");
    expect(out).not.toContain("known memory hit:");
    expect(out).not.toContain("TIP: for file-content search");
    // Everything else survives, including the kinds that share the header.
    expect(out).toContain("RECALLED CONTEXT");
    expect(out).toContain("SYMBOL NUDGE:");
    expect(out).toContain("the content has likely drifted from your last read. Re-read the file");
  });

  it("hides the stale-read sentence even behind a location prefix (documented exception)", () => {
    // The exception both docs name: the stale-read matcher keys on the note's
    // own drift sentence and is NOT line-anchored, so a search/read hit
    // reproducing that sentence in full is hidden despite its `path:line:`
    // prefix. Pinned so the docs cannot drift from the behavior in either
    // direction.
    const hit =
      "src/agent/dispatch.rs: 1568: old_string not found in file — the content has likely drifted from your last read. Re-read the file (read_files, this exact path) and retry with the exact current text; do not edit without a fresh read.";
    expect(stripSteeringNotes(hit, ["edit_stale_read"]).trim()).toBe("");
  });

  it("leaves unrelated notes and plain output untouched", () => {
    const text = [
      "note: content index stale for 19 file(s) — serving tree-walk results",
      "",
      "5 matches in 3 files (engine: walk)",
    ].join("\n");
    expect(stripSteeringNotes(text, ALL_KEYS)).toBe(text);
    expect(stripSteeringNotes("read_files ok\nexit=0", ALL_KEYS)).toBe("read_files ok\nexit=0");
  });

  it("only removes lines that START with the note (mid-line mentions survive)", () => {
    const text = [
      LINES.auto_delegated,
      "  def: a::X::1",
      "the note: AUTO-DELEGATED mention mid-line survives",
      "an AUTO-DELEGATED mention mid-line survives too",
    ].join("\n");
    const out = stripSteeringNotes(text, ["auto_delegated"]);
    expect(out).not.toContain("(re-issue this exact search");
    expect(out).toContain("the note: AUTO-DELEGATED mention mid-line survives");
    expect(out).toContain("an AUTO-DELEGATED mention mid-line survives too");
  });

  it("never drops file content that merely QUOTES a note (location-prefixed lines)", () => {
    // Mirror of the backend's own false-positive guard (steering_stats.rs:
    // a grep result quoting the shell TIP from a file's content must not
    // count as a fired marker — and must not vanish from the display).
    const text = [
      "grep done",
      "src/docs.md: the TIP: for file-content search string quoted from a file",
      "src/steering_stats.rs: const EDIT_STALE_READ_MARK: &str = \"Re-read the file\";",
      "[exit code: 0]",
    ].join("\n");
    expect(stripSteeringNotes(text, ["shell_tip", "edit_stale_read"])).toBe(text);
  });
});

describe("isSteeringNoteHidden", () => {
  it("accepts both the raw line and the note text without its wrapper", () => {
    expect(isSteeringNoteHidden(LINES.literal_tip, ["literal_tip"])).toBe(true);
    expect(
      isSteeringNoteHidden(
        "TIP: pattern has no regex metacharacters — literal:true would use the content-index engine",
        ["literal_tip"],
      ),
    ).toBe(true);
    expect(isSteeringNoteHidden(LINES.literal_tip, [])).toBe(false);
    expect(isSteeringNoteHidden(LINES.literal_tip, ["shell_tip"])).toBe(false);
  });
});

describe("hiddenKeysFromConfig", () => {
  it("falls back to the registry default when the config field is absent", () => {
    expect(hiddenKeysFromConfig(undefined)).toEqual(["auto_delegated"]);
    expect(hiddenKeysFromConfig(null)).toEqual(["auto_delegated"]);
  });

  it("seeds auto_delegated from the legacy toggle when the table is absent", () => {
    // A backend predating the per-kind table still sends the legacy boolean
    // only. Dropping it would silently hide the notes of a user who had
    // turned them on — and that user's next Chat save would persist
    // `auto_delegated: false`, making the loss permanent (closing review
    // LOW 3).
    expect(hiddenKeysFromConfig(undefined, true)).toEqual([]);
    expect(hiddenKeysFromConfig(null, true)).toEqual([]);
    expect(hiddenKeysFromConfig(undefined, false)).toEqual(["auto_delegated"]);
    expect(hiddenKeysFromConfig(null, false)).toEqual(["auto_delegated"]);
    expect(hiddenKeysFromConfig(undefined)).toEqual(["auto_delegated"]);
  });

  it("ignores the legacy toggle once the per-kind table is present", () => {
    const allVisible = Object.fromEntries(STEERING_NOTES.map((d) => [d.key, true]));
    expect(hiddenKeysFromConfig(allVisible, false)).toEqual([]);
    expect(hiddenKeysFromConfig({ ...allVisible, auto_delegated: false }, true)).toEqual([
      "auto_delegated",
    ]);
  });

  it("returns every explicitly-false kind, in registry order", () => {
    const allVisible = Object.fromEntries(STEERING_NOTES.map((d) => [d.key, true]));
    expect(hiddenKeysFromConfig(allVisible)).toEqual([]);
    expect(hiddenKeysFromConfig({ ...allVisible, literal_tip: false, shell_tip: false })).toEqual(
      ["shell_tip", "literal_tip"],
    );
  });

  it("ignores keys that are not steering-note kinds", () => {
    expect(hiddenKeysFromConfig({ nope: false } as never)).toEqual([]);
  });
});

describe("legacy single-toggle wrappers (unchanged behavior)", () => {
  it("stripDelegationNotes still hides both twins, raw and wrapped", () => {
    for (const line of [LINES.auto_delegated, LINES.search_nudge, "AUTO-DELEGATED to memory — 'x' is a known memory hit…"]) {
      expect(stripDelegationNotes(line).trim()).toBe("");
    }
  });

  it("stripDelegationNotes leaves the delegated answer and other notes", () => {
    const text = [
      LINES.auto_delegated,
      "  def: a::X::1",
      "note: content index stale for 19 file(s) — serving tree-walk results",
    ].join("\n");
    const out = stripDelegationNotes(text);
    expect(out).toContain("def: a::X::1");
    expect(out).toContain("content index stale for 19 file(s)");
  });

  it("isDelegationNote recognizes both twins and rejects other notes", () => {
    expect(isDelegationNote("AUTO-DELEGATED to the code graph — 'X' is an indexed symbol (…)")).toBe(true);
    expect(isDelegationNote("AUTO-DELEGATED to memory — 'X' is a known memory hit (…)")).toBe(true);
    expect(isDelegationNote("content index stale for 19 file(s) — serving tree-walk results")).toBe(false);
    expect(isDelegationNote("")).toBe(false);
  });
});

describe("ToolCard render sites apply the filter (source contract)", () => {
  it("the expanded output <pre> routes the result through the per-kind filter", () => {
    expect(messageSource).toContain(
      "{stripSteeringNotes(call.result.output, hiddenSteeringNotes)}",
    );
  });

  it("the shell card routes stdout and stderr through the per-kind filter", () => {
    expect(messageSource).toContain(
      "stripSteeringNotes(shellOut.stdout, hiddenSteeringNotes)",
    );
    expect(messageSource).toContain(
      "stripSteeringNotes(shellOut.stderr, hiddenSteeringNotes)",
    );
  });

  it("the collapsed note chip routes the note through the per-kind filter", () => {
    // Via filterSteeringNote, NOT a boolean guard: a merged note whose hidden
    // component is dropped still shows its visible remainder.
    expect(messageSource).toContain(
      "filterSteeringNote(info.note, hiddenSteeringNotes)",
    );
  });

  it("the collapsed error summary routes through the per-kind filter", () => {
    // Closing review LOW 2: a failed file_edit's output IS the stale-read
    // note line (`toolErrorSummary` returns the first line), so the
    // un-expanded card must filter it exactly like the expanded detail.
    expect(messageSource).toContain(
      "filterSteeringNote(toolErrorSummary(lastCall.result.output), hiddenSteeringNotes)",
    );
    expect(messageSource).toContain('{errorSummary !== "" && (');
  });

  it("both ToolCard and CallDetail read the store's hidden set", () => {
    expect(
      messageSource.match(/useAgentStore\(\(s\) => s\.hiddenSteeringNotes\)/g)?.length,
    ).toBe(2);
  });

  it("no longer consults the removed single-toggle store field", () => {
    expect(messageSource).not.toContain("showDelegationNotes");
  });
});
