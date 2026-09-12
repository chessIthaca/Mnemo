// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * delegationNotes — regression tests for the display-layer filter that hides
 * the search/search_read AUTO-DELEGATION steering note (Chat setting
 * [ui].show_delegation_notes, default off). Pure-helper tests plus static
 * source-contract pins on the ToolCard render sites (vitest runs in `node`
 * env — no React DOM infra), in the style of ChatSection.test.ts.
 */

import { describe, expect, it } from "vitest";
import { isDelegationNote, stripDelegationNotes } from "./delegationNotes";
import messageSource from "../components/chat/Message.tsx?raw";

const CODE_GRAPH_OUTPUT = [
  "note: AUTO-DELEGATED to the code graph — 'ChatDraft' is an indexed symbol (re-issue this exact search to get the plain file search instead):",
  "  def: frontend/src/components/settings/types.ts::ChatDraft::375 (interface, lines 375-384)",
  "  callers (contains): frontend/src/components/settings/types.ts (frontend/src/components/settings/types.ts:1)",
  "  full 360° view: graph_context(id=\"frontend/src/components/settings/types.ts::ChatDraft::375\")",
  "",
  "1 matches in 1 files (engine: index)",
].join("\n");

const MEMORY_OUTPUT = [
  "note: AUTO-DELEGATED to memory — 'endpoint config' is a known memory hit (re-issue this exact search to get the plain file search instead):",
  "  [semantic] SPEC: endpoint config layout (id: …) — endpoints.toml at the project root…",
  "",
  "3 matches in 2 files (engine: walk)",
].join("\n");

/** Fast path (memory hunt, or symbol hunt with no glob): the block is
 *  returned RAW — no `note: ` prefix (search.rs:1328, search_read.rs:235;
 *  pinned by the backend's own tests, e.g. search.rs:2131). */
const FAST_PATH_CODE_GRAPH = [
  "AUTO-DELEGATED to the code graph — 'hello' is an indexed symbol (re-issue this exact search to get the plain file search instead):",
  "  def: a.rs::hello::1 (function, lines 1-1)",
  "  callers: (none — a.rs is the only file)",
  "  full 360° view: graph_context(id=\"a.rs::hello::1\")",
].join("\n");

const FAST_PATH_MEMORY = [
  "AUTO-DELEGATED to memory — 'endpoint config' is a known memory hit (re-issue this exact search to get the plain file search instead):",
  "  [semantic] SPEC: endpoint config layout (id: …) — endpoints.toml at the project root…",
].join("\n");

describe("stripDelegationNotes", () => {
  it("strips the code-graph delegation note line", () => {
    const out = stripDelegationNotes(CODE_GRAPH_OUTPUT);
    expect(out).not.toContain("AUTO-DELEGATED");
    expect(out).not.toContain("note: ");
  });

  it("strips the memory delegation note line", () => {
    const out = stripDelegationNotes(MEMORY_OUTPUT);
    expect(out).not.toContain("AUTO-DELEGATED");
  });

  it("strips the fast-path code-graph header (raw block, no 'note: ' prefix)", () => {
    const out = stripDelegationNotes(FAST_PATH_CODE_GRAPH);
    expect(out).not.toContain("AUTO-DELEGATED");
    expect(out).toContain("def: a.rs::hello::1");
    expect(out).toContain("full 360° view:");
  });

  it("strips the fast-path memory header (raw block, no 'note: ' prefix)", () => {
    const out = stripDelegationNotes(FAST_PATH_MEMORY);
    expect(out).not.toContain("AUTO-DELEGATED");
    expect(out).toContain("[semantic] SPEC: endpoint config layout");
  });

  it("keeps the delegated answer (def:/callers:/full 360° lines)", () => {
    const out = stripDelegationNotes(CODE_GRAPH_OUTPUT);
    expect(out).toContain("def: frontend/src/components/settings/types.ts::ChatDraft::375");
    expect(out).toContain("callers (contains):");
    expect(out).toContain("full 360° view:");
    // The plain-search result lines stay too.
    expect(out).toContain("1 matches in 1 files (engine: index)");
  });

  it("leaves other notes untouched (staleness, reindex, literal-fallback)", () => {
    const text = [
      "note: content index stale for 19 file(s) — serving tree-walk results",
      "",
      "5 matches in 3 files (engine: walk)",
    ].join("\n");
    expect(stripDelegationNotes(text)).toBe(text);
  });

  it("is a no-op on note-free output", () => {
    const text = "read_files ok\nexit=0";
    expect(stripDelegationNotes(text)).toBe(text);
  });

  it("only removes lines that START with a delegation prefix", () => {
    const text = [
      "note: AUTO-DELEGATED to the code graph — 'X' is an indexed symbol (re-issue this exact search to get the plain file search instead):",
      "  def: a::X::1",
      "the note: AUTO-DELEGATED mention mid-line survives",
      "an AUTO-DELEGATED mention mid-line survives too",
    ].join("\n");
    const out = stripDelegationNotes(text);
    expect(out).not.toContain("(re-issue this exact search");
    expect(out).toContain("the note: AUTO-DELEGATED mention mid-line survives");
    expect(out).toContain("an AUTO-DELEGATED mention mid-line survives too");
  });
});

describe("isDelegationNote", () => {
  it("recognizes both twins (note text without the 'note: ' prefix)", () => {
    expect(
      isDelegationNote("AUTO-DELEGATED to the code graph — 'X' is an indexed symbol (…)"),
    ).toBe(true);
    expect(
      isDelegationNote("AUTO-DELEGATED to memory — 'X' is a known memory hit (…)"),
    ).toBe(true);
  });

  it("rejects other notes", () => {
    expect(
      isDelegationNote("content index stale for 19 file(s) — serving tree-walk results"),
    ).toBe(false);
    expect(isDelegationNote("")).toBe(false);
  });
});

describe("ToolCard render sites apply the filter (source contract)", () => {
  it("the expanded output <pre> strips the note when the toggle is off", () => {
    expect(messageSource).toContain(
      "{showDelegationNotes ? call.result.output : stripDelegationNotes(call.result.output)}",
    );
  });

  it("the collapsed note chip skips delegation notes when the toggle is off", () => {
    expect(messageSource).toContain(
      "if (info.note !== null && (showDelegationNotes || !isDelegationNote(info.note))) {",
    );
  });

  it("both ToolCard and CallDetail read the store toggle", () => {
    expect(
      messageSource.match(/useAgentStore\(\(s\) => s\.showDelegationNotes\)/g)?.length,
    ).toBe(2);
  });
});
