// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Direct unit tests for the agentState helpers (quality review LOW 3): the
 * module was extracted from useAgentStore.ts as "fully unit-testable"
 * (Maint H3) but its boundary cases were only exercised indirectly through
 * the 2,192-line table-driven store suite. These pin the helper edges at a
 * fraction of that setup cost: streaming flush, transcript append order,
 * count-cap off-by-ones, entry-id stamping, main-agent selection, the
 * turn-end edge, and the aggregate tok/sec math. (The image byte budget has
 * its own file: agentState.images.test.ts.)
 */

import { describe, expect, it } from "vitest";
import {
  MAX_ACTIVITY_ENTRIES,
  MAX_TRANSCRIPT_ENTRIES,
  aggregateTokPerSec,
  allocEntryId,
  capActivityLog,
  capTranscript,
  didMainTurnEnd,
  emptyAgentState,
  flushStreamingText,
  getOrCreate,
  isActivityEntry,
  isKnowledgeActivityEntry,
  pushTranscriptEntry,
  recentOutputTokPerSec,
  selectMainAgentId,
  stampEntryIds,
} from "./agentState";
import type { ActivityEntry, AgentState, MainRunningSnapshot } from "./agentState";
import type { TranscriptEntry } from "../lib/types";

describe("emptyAgentState + getOrCreate", () => {
  it("empty state has the documented defaults", () => {
    const s = emptyAgentState();
    expect(s.transcript).toEqual([]);
    expect(s.streamingText).toBe("");
    expect(s.running).toBe(false);
    expect(s.failed).toBe(false);
    expect(s.phase).toBe("idle");
    expect(s.pendingApproval).toBeNull();
    expect(s.steers).toEqual([]);
    expect(s.consecutiveToolErrors).toBe(0);
  });

  it("getOrCreate returns the existing state by reference", () => {
    const existing = emptyAgentState();
    const agents: Record<number, AgentState> = { 1: existing };
    expect(getOrCreate(agents, 1)).toBe(existing);
  });

  it("getOrCreate creates a fresh state on first touch without inserting it", () => {
    const agents: Record<number, AgentState> = {};
    const created = getOrCreate(agents, 7);
    expect(created).toEqual(emptyAgentState());
    // Insertion into the map is the caller's job — the helper stays pure.
    expect(agents[7]).toBeUndefined();
  });
});

describe("flushStreamingText", () => {
  it("is a no-op when nothing is streaming", () => {
    const s = emptyAgentState();
    const transcriptBefore = s.transcript;
    flushStreamingText(s);
    expect(s.transcript).toBe(transcriptBefore);
    expect(s.streamingText).toBe("");
  });

  it("moves the streamed text into the transcript as an assistant entry and clears it", () => {
    const s = emptyAgentState();
    s.transcript = [{ kind: "user", text: "q" }];
    s.streamingText = "the answer";
    flushStreamingText(s);
    expect(s.transcript).toEqual([
      { kind: "user", text: "q" },
      { kind: "assistant", text: "the answer" },
    ]);
    expect(s.streamingText).toBe("");
  });
});

describe("pushTranscriptEntry", () => {
  it("flushes pending streaming text first — the entry lands ABOVE it", () => {
    // The load-bearing order: text that streamed BEFORE a tool call must
    // land ABOVE the tool card, or tools appear to jump to the top.
    const agent = emptyAgentState();
    agent.streamingText = "partial answer";
    pushTranscriptEntry(agent, { kind: "tool", name: "shell", calls: [] });
    expect(agent.transcript.map((e) => e.kind)).toEqual(["assistant", "tool"]);
    expect(agent.streamingText).toBe("");
  });

  it("appends without flushing when nothing is streaming", () => {
    const agent = emptyAgentState();
    pushTranscriptEntry(agent, { kind: "steer", text: "go" });
    expect(agent.transcript).toEqual([{ kind: "steer", text: "go" }]);
  });
});

describe("capTranscript count-cap edges", () => {
  it("empty transcript stays empty (same reference)", () => {
    const entries: TranscriptEntry[] = [];
    expect(capTranscript(entries)).toBe(entries);
  });

  it("exactly MAX_TRANSCRIPT_ENTRIES is a no-op (same reference)", () => {
    const entries: TranscriptEntry[] = [];
    for (let i = 0; i < MAX_TRANSCRIPT_ENTRIES; i++) {
      entries.push({ kind: "assistant", text: `t${i}` });
    }
    expect(capTranscript(entries)).toBe(entries);
  });

  it("MAX + 1 drops exactly the oldest entry (off-by-one)", () => {
    const entries: TranscriptEntry[] = [];
    for (let i = 0; i < MAX_TRANSCRIPT_ENTRIES + 1; i++) {
      entries.push({ kind: "assistant", text: `t${i}` });
    }
    const result = capTranscript(entries);
    expect(result.length).toBe(MAX_TRANSCRIPT_ENTRIES);
    // t0 is dropped; the window keeps t1..t1000.
    expect(result[0]).toEqual({ kind: "assistant", text: "t1" });
    expect(result[result.length - 1]).toEqual({
      kind: "assistant",
      text: `t${MAX_TRANSCRIPT_ENTRIES}`,
    });
  });
});

describe("capActivityLog", () => {
  it("no-op at the cap, drops the oldest one over it", () => {
    const atCap: ActivityEntry[] = [];
    for (let i = 0; i < MAX_ACTIVITY_ENTRIES; i++) {
      atCap.push({ kind: "info", text: `i${i}`, timestamp: i });
    }
    expect(capActivityLog(atCap)).toBe(atCap);
    const over: ActivityEntry[] = [...atCap, { kind: "error", text: "newest", timestamp: 999 }];
    const result = capActivityLog(over);
    expect(result.length).toBe(MAX_ACTIVITY_ENTRIES);
    expect(result[0]).toEqual({ kind: "info", text: "i1", timestamp: 1 });
    expect(result[result.length - 1]).toEqual({
      kind: "error",
      text: "newest",
      timestamp: 999,
    });
  });
});

describe("entry ids (allocEntryId + stampEntryIds)", () => {
  it("allocEntryId is monotonic", () => {
    // Relative assertions only — the module counter is file-local state and
    // other tests in this file may have advanced it.
    const a = allocEntryId();
    const b = allocEntryId();
    const c = allocEntryId();
    expect(b).toBe(a + 1);
    expect(c).toBe(b + 1);
  });

  it("stampEntryIds returns the same array when every entry is stamped", () => {
    const entries: TranscriptEntry[] = [
      { kind: "assistant", text: "a", entryId: 10 },
      { kind: "user", text: "b", entryId: 11 },
    ];
    expect(stampEntryIds(entries)).toBe(entries);
  });

  it("stampEntryIds stamps only the entries lacking an id", () => {
    const stamped: TranscriptEntry = { kind: "assistant", text: "a", entryId: 10 };
    const bare: TranscriptEntry = { kind: "user", text: "b" };
    const result = stampEntryIds([stamped, bare]);
    expect(result[0]).toBe(stamped);
    expect(result[1].entryId).toBeDefined();
  });

  it("stampEntryIds syncs the counter past foreign ids (no collisions on /load)", () => {
    // /load is the only path importing entries with pre-existing entryIds;
    // without the sync the next allocation could collide with a loaded id.
    const foreign = 1_000_000;
    const loaded: TranscriptEntry[] = [
      { kind: "assistant", text: "restored", entryId: foreign },
    ];
    expect(stampEntryIds(loaded)).toBe(loaded);
    expect(allocEntryId()).toBeGreaterThan(foreign);
  });
});

describe("selectMainAgentId", () => {
  it("prefers the smallest parentless id", () => {
    const parents: Record<number, number | null> = { 1: null, 2: 1, 3: null };
    expect(selectMainAgentId(parents, {})).toBe(1);
  });

  it("falls back to the smallest known id while parent info loads", () => {
    const parents: Record<number, number | null> = { 2: 1 };
    expect(
      selectMainAgentId(parents, { 2: emptyAgentState(), 5: emptyAgentState() }),
    ).toBe(2);
  });

  it("returns null when nothing is known", () => {
    expect(selectMainAgentId({}, {})).toBeNull();
  });
});

describe("didMainTurnEnd", () => {
  const snap = (running: boolean): MainRunningSnapshot => ({
    agentParents: { 1: null },
    agents: { 1: { ...emptyAgentState(), running } },
  });

  it("fires only on the running true→false edge", () => {
    expect(didMainTurnEnd(snap(true), snap(false))).toBe(true);
    expect(didMainTurnEnd(snap(false), snap(false))).toBe(false);
    expect(didMainTurnEnd(snap(false), snap(true))).toBe(false);
    expect(didMainTurnEnd(snap(true), snap(true))).toBe(false);
  });

  it("a missing main agent reads as not running", () => {
    const empty: MainRunningSnapshot = { agentParents: {}, agents: {} };
    expect(didMainTurnEnd(snap(true), empty)).toBe(true);
    expect(didMainTurnEnd(empty, snap(true))).toBe(false);
  });
});

describe("aggregateTokPerSec", () => {
  it("returns null with no timing data", () => {
    expect(aggregateTokPerSec(1000, 0)).toBeNull();
  });

  it("divides total tokens by total time (never by the request count)", () => {
    // 10 requests of 100 tokens / 1000ms each: 100 tok/s — NOT 1000.
    expect(aggregateTokPerSec(10 * 100, 10 * 1000)).toBeCloseTo(100);
  });
});

describe("recentOutputTokPerSec", () => {
  it("returns null when the window is empty", () => {
    expect(recentOutputTokPerSec(emptyAgentState().sessionTiming)).toBeNull();
  });

  it("rates the window total-over-total (never average-of-rates)", () => {
    // Three samples with DIFFERING per-request rates: 100 tok/s, 33.3 tok/s,
    // 16.7 tok/s. Total-over-total = 300 tok / 10 s = 30 tok/s; the average
    // of the per-request rates would be 50 — the N× inflation lesson from
    // 463ddd4, applied to the window.
    const st = emptyAgentState().sessionTiming;
    st.recentTiming = [
      { completion_tokens: 100, generation_ms: 1000 },
      { completion_tokens: 100, generation_ms: 3000 },
      { completion_tokens: 100, generation_ms: 6000 },
    ];
    expect(recentOutputTokPerSec(st)).toBeCloseTo(30);
  });
});

describe("activity classification", () => {
  it("isActivityEntry: tool/memory/vision/skill cards only", () => {
    expect(isActivityEntry({ kind: "tool", name: "shell", calls: [] })).toBe(true);
    expect(
      isActivityEntry({ kind: "memory", name: "memory_search", success: true, running: false }),
    ).toBe(true);
    expect(
      isActivityEntry({
        kind: "vision",
        index: 0,
        total: 1,
        query: "q",
        description: null,
        success: false,
        running: true,
      }),
    ).toBe(true);
    expect(isActivityEntry({ kind: "skill", name: "s", prompt: "p" })).toBe(true);
    expect(isActivityEntry({ kind: "assistant", text: "t" })).toBe(false);
    expect(isActivityEntry({ kind: "user", text: "t" })).toBe(false);
    expect(isActivityEntry({ kind: "error", text: "t" })).toBe(false);
    expect(isActivityEntry({ kind: "steer", text: "t" })).toBe(false);
    expect(isActivityEntry({ kind: "qa", question: "q", answer: "a" })).toBe(false);
  });

  it("isKnowledgeActivityEntry: memory + graph_* tools only", () => {
    expect(
      isKnowledgeActivityEntry({
        kind: "memory",
        name: "auto-recall",
        success: true,
        running: false,
      }),
    ).toBe(true);
    expect(isKnowledgeActivityEntry({ kind: "tool", name: "graph_search", calls: [] })).toBe(true);
    expect(isKnowledgeActivityEntry({ kind: "tool", name: "shell", calls: [] })).toBe(false);
    expect(
      isKnowledgeActivityEntry({
        kind: "vision",
        index: 0,
        total: 1,
        query: "q",
        description: null,
        success: false,
        running: true,
      }),
    ).toBe(false);
  });
});
