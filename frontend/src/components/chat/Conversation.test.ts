// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Conversation transcript filter (backlog db489070) — the show_tool_activity
 * GUI-only display gate. Static source-contract tests in the style of
 * ChatSection.test.ts (vitest runs in `node` env — no React DOM infra), plus
 * pure-helper tests for the isActivityEntry predicate the filter uses.
 *
 * Contract being pinned: when the toggle is off (the default), agent-activity
 * cards (tool/memory/vision/skill) are skipped at RENDER time; the transcript
 * store keeps every entry either way (the model's context echo is built
 * server-side and is unaffected), and the Output tab + console still log
 * every tool call.
 */

import { describe, expect, it } from "vitest";
import conversationSource from "./Conversation.tsx?raw";
import { isActivityEntry, isKnowledgeActivityEntry } from "../../hooks/agentState";
import type { TranscriptEntry } from "../../lib/types";

describe("Conversation show_tool_activity render filter", () => {
  it("reads the global showToolActivity flag from the store", () => {
    expect(conversationSource).toContain(
      "useAgentStore((s) => s.showToolActivity)",
    );
  });

  it("skips activity entries at render time via isActivityEntry (filter form)", () => {
    expect(conversationSource).toContain("isActivityEntry(entry)");
    // Hidden entries are filtered out before render (the simple version —
    // no "N tool calls" summary line): the InflightBar shows live activity
    // during a turn and the Output tab holds the full log.
    expect(conversationSource).toContain(
      "const visible = state.transcript.filter(",
    );
  });

  it("is a GUI-only filter — no reducer/store suppression of entries", () => {
    // The old showMemoryActivity reducer gate suppressed memory entries at
    // the store; that path is gone. Conversation must not reference it.
    expect(conversationSource).not.toContain("showMemoryActivity");
  });
});

describe("Conversation show_knowledge_activity render filter (backlog 68c4c9a5)", () => {
  it("reads the global showKnowledgeActivity flag from the store", () => {
    expect(conversationSource).toContain(
      "useAgentStore((s) => s.showKnowledgeActivity)",
    );
  });

  it("exempts knowledge cards from the show_tool_activity gate via isKnowledgeActivityEntry", () => {
    // The render filter: showToolActivity || !isActivityEntry(entry) ||
    // (showKnowledgeActivity && isKnowledgeActivityEntry(entry))
    expect(conversationSource).toContain("isKnowledgeActivityEntry(entry)");
  });
});

describe("Conversation transcript grouping memoization (mem-perf review HIGH 2)", () => {
  it("memoizes the filter+window+group+chunk pass, keyed on the transcript's identity", () => {
    // Conversation re-renders on every streaming frame (the state prop's
    // identity changes per rAF flush), but the transcript ARRAY keeps its
    // identity while text streams into state.streamingText — so a memo
    // keyed on it skips the entire O(transcript) pass in the common case
    // (text streaming into the last turn). The toggles are keys too:
    // flipping one re-filters. The render window is a key as well —
    // growing it via the expander re-windows.
    expect(conversationSource).toContain("const { turns, hidden } = useMemo(");
    expect(conversationSource).toContain(
      "[state.transcript, showToolActivity, showKnowledgeActivity, windowSize]",
    );
  });

  it("the render consumes the pre-chunked turns — chunkRuns is not re-run per turn per render", () => {
    // chunkRuns is precomputed inside the memo; re-running it in the
    // render body would restore the per-frame O(transcript) cost.
    expect(conversationSource).toContain("turn.map((run, ri)");
    expect(conversationSource).not.toContain("chunkRuns(turn).map(");
  });
});

describe("Conversation stable keys (mem-perf review HIGH 3)", () => {
  it("turns, runs, and entries are keyed by the stable entryId, not index", () => {
    // Index keys misattribute row state once the transcript hits its
    // 1000-entry cap: every append shifts all indices, so a Message row's
    // key lands on a DIFFERENT entry (arePropsEqual fails → deep
    // re-render; local row state like an expanded tool card silently
    // transfers). Keys are the monotonic entryId stamped at creation; the
    // position fallback only covers id-less legacy entries.
    expect(conversationSource).toContain("key={turn[0][0].entryId ?? `t${ti}`}");
    expect(conversationSource).toContain("key={run[0].entryId ?? `r${ri}`}");
    expect(conversationSource).toContain("renderEntry(e, e.entryId ?? `i${i}`)");
    expect(conversationSource).toContain("renderEntry(run[0], run[0].entryId ?? `r${ri}`)");
  });

  it("no bare index keys remain in the turn/run lists", () => {
    expect(conversationSource).not.toContain("key={ti}");
    expect(conversationSource).not.toContain("key={ri}");
  });
});

describe("isKnowledgeActivityEntry (the show_knowledge_activity predicate)", () => {
  it("graph_* tool calls are knowledge activity", () => {
    const graphTools: TranscriptEntry[] = [
      { kind: "tool", name: "graph_search", calls: [] },
      { kind: "tool", name: "graph_context", calls: [] },
      { kind: "tool", name: "graph_impact", calls: [] },
      { kind: "tool", name: "graph_path", calls: [] },
    ];
    expect(graphTools.every(isKnowledgeActivityEntry)).toBe(true);
  });

  it("memory entries (tool calls + auto-recall) are knowledge activity", () => {
    const memoryEntries: TranscriptEntry[] = [
      {
        kind: "memory",
        name: "memory_search",
        id: "c1",
        index: 0,
        args: "",
        tier: undefined,
        title: undefined,
        snippet: undefined,
        success: true,
        running: false,
      },
      {
        kind: "memory",
        name: "auto-recall",
        id: "c2",
        index: 1,
        args: "",
        tier: undefined,
        title: undefined,
        snippet: "3 hit(s)",
        success: true,
        running: false,
      },
    ];
    expect(memoryEntries.every(isKnowledgeActivityEntry)).toBe(true);
  });

  it("non-knowledge activity is NOT knowledge activity", () => {
    const nonKnowledge: TranscriptEntry[] = [
      { kind: "tool", name: "shell", calls: [] },
      { kind: "tool", name: "read_files", calls: [] },
      { kind: "tool", name: "file_edit", calls: [] },
      {
        kind: "vision",
        index: 1,
        total: 1,
        query: "q",
        description: null,
        success: true,
        running: false,
      },
      { kind: "skill", name: "merge_to_main", prompt: "p" },
    ];
    expect(nonKnowledge.every((e) => !isKnowledgeActivityEntry(e))).toBe(true);
  });

  it("conversation kinds are NOT knowledge activity", () => {
    const conversation: TranscriptEntry[] = [
      { kind: "user", text: "hi" },
      { kind: "assistant", text: "hello" },
      { kind: "error", text: "boom" },
    ];
    expect(conversation.every((e) => !isKnowledgeActivityEntry(e))).toBe(true);
  });
});

describe("isActivityEntry (the show_tool_activity predicate)", () => {
  it("activity kinds are gated: tool / memory / vision / skill", () => {
    const activity: TranscriptEntry[] = [
      { kind: "tool", name: "shell", calls: [] },
      {
        kind: "memory",
        name: "memory_search",
        id: "c1",
        index: 0,
        args: "",
        tier: undefined,
        title: undefined,
        snippet: undefined,
        success: true,
        running: false,
      },
      {
        kind: "vision",
        index: 1,
        total: 1,
        query: "q",
        description: null,
        success: true,
        running: false,
      },
      { kind: "skill", name: "merge_to_main", prompt: "p" },
    ];
    expect(activity.every(isActivityEntry)).toBe(true);
  });

  it("conversation kinds always render: user / assistant / error / steer / qa", () => {
    const conversation: TranscriptEntry[] = [
      { kind: "user", text: "hi" },
      { kind: "assistant", text: "hello" },
      { kind: "error", text: "boom" },
      { kind: "steer", text: "do this instead" },
      { kind: "qa", question: "Q?", answer: "A" },
    ];
    expect(conversation.every((e) => !isActivityEntry(e))).toBe(true);
  });
});
