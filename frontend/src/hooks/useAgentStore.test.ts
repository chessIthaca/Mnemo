// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Table-driven unit tests for the pure per-event reducers in
 * `useAgentStore.ts` (via `applyAgentEvent` in `agentEventReducer.ts`).
 *
 * Every `SerializableAgentEvent` kind is exercised through the store's public
 * `handleAgentEvent` dispatch path (a thin wrapper over `applyAgentEvent`),
 * asserting the resulting state slice. Side-effecting behaviors (the
 * landed-steer removal timer) are tested through their effects contract.
 */

import { describe, expect, it, beforeEach } from "vitest";

import { useAgentStore, aggregateTokPerSec, didMainTurnEnd, emptyAgentState, MAX_ACTIVITY_ENTRIES, MAX_TRANSCRIPT_ENTRIES, MAX_CALLS_PER_TOOL_CARD, MAX_PLAN_DIFFS, recentOutputTokPerSec } from "./useAgentStore";
import { applyAgentEvent, DOOM_ERROR_STREAK, isUserDenialToolOutput } from "./agentEventReducer";
import type { AgentId, TranscriptEntry } from "../lib/types";
import type { MainRunningSnapshot } from "./agentState";
import { allocEntryId, stampEntryIds } from "./agentState";

/** Reset the store slices the reducers touch before each test. */
function resetStore(): void {
  useAgentStore.setState({
    agents: {},
    agentNames: {},
    agentParents: {},
    agentModels: {},
    agentEfforts: {},
    agentWireEfforts: {},
    agentProviders: {},
    workflowStates: {},
    activeAgent: null,
    lastDiff: null,
    planDiffs: [],
    topPlanId: null,
    selectedDiffPath: null,
    planVersion: 0,
    backlog: [],
    autoFeed: false,
    runAll: { active: false, done: 0, total: 0, compacting: false, concurrency: 1, spawned: [], note: null },
    backlogDraft: "",
    backlogDraftImages: [],
    showToolActivity: true,
    showKnowledgeActivity: true,
    chatThreadLine: true,
    chatProseCap: true,
    chatTurnTint: true,
    chatHoverTimestamps: true,
  });
}

function agent(id: AgentId) {
  const a = useAgentStore.getState().agents[id];
  if (!a) throw new Error(`agent ${id} missing`);
  return a;
}

const ID = 1 as AgentId;
const SUB = 2 as AgentId;

describe("agent event reducers (table-driven)", () => {
  beforeEach(resetStore);

  it("started: resets per-turn state and marks running", () => {
    useAgentStore.setState((s) => ({
      agents: {
        ...s.agents,
        [ID]: {
          ...useAgentStore.getState().agents[ID]!,
          running: false,
          tokenUsage: { prompt: 5, completion: 5, reasoning: 1, cached: 2 },
          streamingText: "stale",
          activityLog: [{ kind: "info", text: "x", timestamp: 1 }],
        },
      },
    }));
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    const a = agent(ID);
    expect(a.running).toBe(true);
    expect(a.tokenUsage).toEqual({ prompt: 0, completion: 0, reasoning: 0, cached: 0 });
    expect(a.streamingText).toBe("");
    expect(a.activityLog).toEqual([]);
    // Fresh turn: phase flips to sending + the elapsed timer starts.
    expect(a.phase).toBe("sending");
    expect(a.turnStartedAt).not.toBeNull();
    expect(a.liveCompletionTokens).toBe(0);
    expect(a.liveReasoningTokens).toBe(0);
  });

  it("started: a mid-turn provider-retry re-Start keeps the original timer", () => {
    // Attempt 1 starts the turn.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    const firstStart = agent(ID).turnStartedAt;
    expect(firstStart).not.toBeNull();
    // A retrying error keeps running=true, then the retry re-emits started.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "error", error: "transient", retrying: true } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    const a = agent(ID);
    expect(a.running).toBe(true);
    expect(a.turnStartedAt).toBe(firstStart);
    expect(a.phase).toBe("sending");
  });

  it("phase: updates the live turn phase", () => {
    for (const phase of ["sending", "compacting", "waiting", "reasoning", "streaming", "running_tools"] as const) {
      useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "phase", phase } });
      expect(agent(ID).phase).toBe(phase);
    }
  });

  it("liveTokens: deltas accumulate chars/4 estimates per bucket and usage snaps them to zero", () => {
    // 8 chars → 2 estimated tokens, completion bucket.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "12345678" } });
    expect(agent(ID).liveCompletionTokens).toBe(2);
    // Reasoning counts into its OWN bucket — the 🧠 counter climbs during
    // the thinking phase instead of sitting at 0 until usage lands.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "reasoning_delta", text: "abcd" } });
    expect(agent(ID).liveReasoningTokens).toBe(1);
    expect(agent(ID).liveCompletionTokens).toBe(2);
    // The authoritative usage arrives → estimates reset, real tokens land in
    // tokenUsage.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "usage",
        prompt_tokens: 10,
        completion_tokens: 30,
        reasoning_tokens: 5,
        cached_tokens: 0,
        ttft_ms: 100,
        generation_ms: 400,
      },
    });
    const a = agent(ID);
    expect(a.liveCompletionTokens).toBe(0);
    expect(a.liveReasoningTokens).toBe(0);
    expect(a.tokenUsage.completion).toBe(30);
    expect(a.tokenUsage.reasoning).toBe(5);
  });

  it("finished: goes idle, clears the timer + estimates", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "hello world" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "finished", reason: "stop" } });
    const a = agent(ID);
    expect(a.phase).toBe("idle");
    expect(a.turnStartedAt).toBeNull();
    expect(a.liveCompletionTokens).toBe(0);
    expect(a.liveReasoningTokens).toBe(0);
    expect(a.running).toBe(false);
  });

  it("text_delta: appends to streaming text", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "hel" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "lo" } });
    expect(agent(ID).streamingText).toBe("hello");
  });

  it("reasoning_delta: appends to streaming reasoning + activity log", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "reasoning_delta", text: "think" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "reasoning_delta", text: "ing" } });
    const a = agent(ID);
    expect(a.streamingReasoning).toBe("thinking");
    expect(a.activityLog).toHaveLength(1);
    expect(a.activityLog[0].kind).toBe("reasoning");
    expect(a.activityLog[0].text).toBe("thinking");
  });

  it("reasoning_delta: new reasoning after a non-reasoning entry replaces the log", () => {
    // Seed a non-reasoning activity entry (as after a tool result), then a new
    // reasoning delta — the reducer clears the log when the last entry isn't
    // reasoning, so the previous turn's reasoning is replaced.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "reasoning_delta", text: "old reasoning" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "error", error: "transient", retrying: true } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "reasoning_delta", text: "new reasoning" } });
    const a = agent(ID);
    expect(a.activityLog).toHaveLength(1);
    expect(a.activityLog[0].kind).toBe("reasoning");
    expect(a.activityLog[0].text).toBe("new reasoning");
  });

  it("reasoning_delta: appends to the last entry when it is already reasoning", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "reasoning_delta", text: "part1 " } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "reasoning_delta", text: "part2" } });
    const a = agent(ID);
    expect(a.activityLog).toHaveLength(1);
    expect(a.activityLog[0].text).toBe("part1 part2");
  });

  it("text_delta: mirrors the answer into the activity log (coalescing)", () => {
    // Regression (2026-08-22): the completion must stream into the reasoning
    // box too — consecutive chunks coalesce into one "answer" entry.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "Hel" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "lo" } });
    const a = agent(ID);
    expect(a.streamingText).toBe("Hello");
    expect(a.activityLog).toHaveLength(1);
    expect(a.activityLog[0].kind).toBe("answer");
    expect(a.activityLog[0].text).toBe("Hello");
  });

  it("text_delta: reasoning → answer keeps both entries in order", () => {
    // The box shows the thinking first, then the answer below it.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "reasoning_delta", text: "think" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "answer" } });
    const a = agent(ID);
    expect(a.activityLog.map((e) => e.kind)).toEqual(["reasoning", "answer"]);
    expect(a.activityLog[1].text).toBe("answer");
  });

  it("text_delta: a following reasoning_delta replaces the log (existing rule)", () => {
    // The clear-on-new-reasoning rule is preserved verbatim: the next turn's
    // first reasoning delta replaces the previous reasoning+answer.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "old answer" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "reasoning_delta", text: "new reasoning" } });
    const a = agent(ID);
    expect(a.activityLog).toHaveLength(1);
    expect(a.activityLog[0].kind).toBe("reasoning");
    expect(a.activityLog[0].text).toBe("new reasoning");
  });

  it("tool_call_start: pushes a tool transcript entry", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
    });
    const a = agent(ID);
    expect(a.transcript).toHaveLength(1);
    const t = a.transcript[0];
    expect(t.kind).toBe("tool");
    if (t.kind === "tool") {
      expect(t.name).toBe("shell");
      expect(t.calls).toEqual([{ id: "call-1", index: 0, args: "", result: null }]);
    }
  });

  it("tool_call_start: flushes pre-tool text above the card", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "let me run" } });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
    });
    const a = agent(ID);
    expect(a.streamingText).toBe("");
    expect(a.transcript[0]).toEqual({ kind: "assistant", text: "let me run", ts: expect.any(Number), entryId: expect.any(Number) });
    expect(a.transcript[1].kind).toBe("tool");
  });

  it("tool_call_start: merges consecutive same-tool calls into one card", () => {
    // Generic tools still merge (shell/browser/search are the never-merge
    // exception, covered by the dedicated tests below — backlog daa38cbe +
    // 24ddb845).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "file_edit" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 1, id: "call-2", name: "file_edit" },
    });
    const a = agent(ID);
    const tool = a.transcript.find((e) => e.kind === "tool");
    expect(tool).toBeDefined();
    if (tool && tool.kind === "tool") expect(tool.calls).toHaveLength(2);
  });

  it("tool_call_start: a failed call breaks the merge chain (error + success stay separate)", () => {
    // call-1 fails, then call-2 (same tool) starts: the failed call must NOT
    // merge with the retry — they land in separate cards.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "file_edit" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "tool_result",
        tool_call_id: "call-1",
        result: { success: false, output: "error: no matches" },
      },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 1, id: "call-2", name: "file_edit" },
    });
    const tools = agent(ID).transcript.filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(2);
    if (tools[0].kind === "tool") expect(tools[0].calls).toHaveLength(1);
    if (tools[1].kind === "tool") expect(tools[1].calls).toHaveLength(1);
  });

  it("tool_call_start: a successful call keeps the merge chain alive", () => {
    // call-1 succeeds, then call-2 (same tool) starts: they merge into one
    // card (the success does not break the chain).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "file_edit" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "tool_result",
        tool_call_id: "call-1",
        result: { success: true, output: "found 3" },
      },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 1, id: "call-2", name: "file_edit" },
    });
    const tools = agent(ID).transcript.filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(1);
    if (tools[0].kind === "tool") expect(tools[0].calls).toHaveLength(2);
  });

  it("shell calls never merge: sequential calls stay on separate lines", () => {
    // Shell commands never group (backlog daa38cbe): the command of each call
    // is the point of the card. call→result→call stays TWO cards, one call
    // each — same-tool non-shell tools would merge into one card with two
    // calls (see the merge-chain test above).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "tool_result",
        tool_call_id: "call-1",
        result: { success: true, output: "ok" },
      },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 1, id: "call-2", name: "shell" },
    });
    const tools = agent(ID).transcript.filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(2);
    if (tools[0].kind === "tool") {
      expect(tools[0].name).toBe("shell");
      expect(tools[0].calls).toHaveLength(1);
    }
    if (tools[1].kind === "tool") {
      expect(tools[1].name).toBe("shell");
      expect(tools[1].calls).toHaveLength(1);
    }
  });

  it("shell calls never merge even within one parallel batch", () => {
    // Two tool_call_start(shell) with no intervening result would merge for
    // any other tool; shell always renders one call per card (backlog daa38cbe).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 1, id: "call-2", name: "shell" },
    });
    const tools = agent(ID).transcript.filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(2);
    if (tools[0].kind === "tool") expect(tools[0].calls).toHaveLength(1);
    if (tools[1].kind === "tool") expect(tools[1].calls).toHaveLength(1);
  });

  it("browser_navigate calls never merge: each action is its own line", () => {
    // The live-tab browser_* family never groups (backlog daa38cbe): each
    // action's target is the point of the card.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "browser_navigate" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "tool_result",
        tool_call_id: "call-1",
        result: { success: true, output: "navigated to https://example.com" },
      },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 1, id: "call-2", name: "browser_navigate" },
    });
    const tools = agent(ID).transcript.filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(2);
    if (tools[0].kind === "tool") {
      expect(tools[0].name).toBe("browser_navigate");
      expect(tools[0].calls).toHaveLength(1);
    }
    if (tools[1].kind === "tool") {
      expect(tools[1].name).toBe("browser_navigate");
      expect(tools[1].calls).toHaveLength(1);
    }
  });

  it("offscreen_browser_navigate calls never merge: the headless family too", () => {
    // The headless offscreen_browser_* family is in the never-merge set
    // (backlog daa38cbe).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "offscreen_browser_navigate" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "tool_result",
        tool_call_id: "call-1",
        result: { success: true, output: "navigated to https://example.com" },
      },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 1, id: "call-2", name: "offscreen_browser_navigate" },
    });
    const tools = agent(ID).transcript.filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(2);
    if (tools[0].kind === "tool") {
      expect(tools[0].name).toBe("offscreen_browser_navigate");
      expect(tools[0].calls).toHaveLength(1);
    }
    if (tools[1].kind === "tool") {
      expect(tools[1].name).toBe("offscreen_browser_navigate");
      expect(tools[1].calls).toHaveLength(1);
    }
  });

  it("search calls never merge even within one parallel batch", () => {
    // Two tool_call_start(search) with no intervening result would merge for
    // any other tool; each search query is distinct and must render one call
    // per card (backlog 24ddb845, extending the daa38cbe directive).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "search" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 1, id: "call-2", name: "search" },
    });
    const tools = agent(ID).transcript.filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(2);
    if (tools[0].kind === "tool") expect(tools[0].calls).toHaveLength(1);
    if (tools[1].kind === "tool") expect(tools[1].calls).toHaveLength(1);
  });

  it("search_read calls never merge: the auto-read variant too", () => {
    // search_read shares the search never-merge set (backlog 24ddb845):
    // each query is distinct even when the calls run back to back.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "search_read" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 1, id: "call-2", name: "search_read" },
    });
    const tools = agent(ID).transcript.filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(2);
    if (tools[0].kind === "tool") {
      expect(tools[0].name).toBe("search_read");
      expect(tools[0].calls).toHaveLength(1);
    }
    if (tools[1].kind === "tool") {
      expect(tools[1].name).toBe("search_read");
      expect(tools[1].calls).toHaveLength(1);
    }
  });

  it("sequential search calls stay separate cards even after a result", () => {
    // call → result → call: the completed result must not let the next
    // search merge into the first card (backlog 24ddb845).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "search" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "tool_result",
        tool_call_id: "call-1",
        result: { success: true, output: "1 match" },
      },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 1, id: "call-2", name: "search" },
    });
    const tools = agent(ID).transcript.filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(2);
    if (tools[0].kind === "tool") expect(tools[0].calls).toHaveLength(1);
    if (tools[1].kind === "tool") expect(tools[1].calls).toHaveLength(1);
  });

  it("regression (review M2): same-tool runs split past MAX_CALLS_PER_TOOL_CARD and split cards still resolve results", () => {
    // A long same-name run: MAX_CALLS_PER_TOOL_CARD merge into the first
    // card, the remainder must open a NEW card (the old code merged all into
    // one entry whose calls array — args + outputs — grew without bound).
    const total = MAX_CALLS_PER_TOOL_CARD + 10;
    for (let i = 0; i < total; i++) {
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: { kind: "tool_call_start", index: i, id: `call-${i}`, name: "file_edit" },
      });
    }
    const tools = agent(ID).transcript.filter((e) => e.kind === "tool");
    expect(tools).toHaveLength(2);
    if (tools[0].kind === "tool") {
      expect(tools[0].calls).toHaveLength(MAX_CALLS_PER_TOOL_CARD);
    }
    if (tools[1].kind === "tool") {
      expect(tools[1].calls).toHaveLength(total - MAX_CALLS_PER_TOOL_CARD);
    }

    // A result for a call in the SECOND card updates that card's call in
    // place (reduceToolResult scans all entries by call id).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "tool_result",
        tool_call_id: `call-${total - 1}`,
        result: { success: true, output: "found" },
      },
    });
    const second = agent(ID).transcript.filter((e) => e.kind === "tool")[1];
    if (second.kind === "tool") {
      const call = second.calls.find((c) => c.id === `call-${total - 1}`);
      expect(call?.result).toEqual({ success: true, output: "found" });
    }
  });

  it("regression: a foreign tool's result must not be stolen by a running memory entry (git_read stuck \"running\")", () => {
    // User report 2026-08-25 (backlog 12d7ecb8): in a parallel block
    // [git_read, memory_search, search], the git_read card stayed "running"
    // forever while memory_search got its ✓. Root cause: reduceToolResult's
    // memory-finalization loop matched ANY running memory entry on ANY
    // tool_result (no tool_call_id correlation) and then skipped the
    // tool-card loop — git_read's result was consumed finalizing the
    // memory_search entry (with garbage-parsed tier/title). Memory entries
    // now always enter the store (visibility is a render-time concern), so
    // this runs with no flag.
    const dispatch = useAgentStore.getState().handleAgentEvent;
    dispatch({ agent_id: ID, event: { kind: "tool_call_start", index: 0, id: "call-g", name: "git_read" } });
    dispatch({ agent_id: ID, event: { kind: "tool_call_start", index: 1, id: "call-m", name: "memory_search" } });
    dispatch({ agent_id: ID, event: { kind: "tool_call_start", index: 2, id: "call-s", name: "search" } });
    // git_read's result arrives FIRST — it must attach to the git_read card,
    // not finalize the running memory_search entry.
    dispatch({
      agent_id: ID,
      event: {
        kind: "tool_result",
        tool_call_id: "call-g",
        result: { success: true, output: "$ git log\nabc123 def" },
      },
    });
    const git = agent(ID).transcript.find((e) => e.kind === "tool" && e.name === "git_read");
    if (!git || git.kind !== "tool") throw new Error("git_read tool entry missing");
    expect(git.calls[0].result).toEqual({ success: true, output: "$ git log\nabc123 def" });
    const mem = agent(ID).transcript.find((e) => e.kind === "memory");
    if (!mem || mem.kind !== "memory") throw new Error("memory entry missing");
    expect(mem.running).toBe(true);
    // The memory_search entry finalizes only on ITS OWN result.
    dispatch({
      agent_id: ID,
      event: {
        kind: "tool_result",
        tool_call_id: "call-m",
        result: {
          success: true,
          output:
            "1 memories matched:\n\n[semantic] merge notes (id: x, score: 0.9, strength: 1.00)\n  note body\n\n",
        },
      },
    });
    const mem2 = agent(ID).transcript.find((e) => e.kind === "memory");
    if (!mem2 || mem2.kind !== "memory") throw new Error("memory entry missing");
    expect(mem2.running).toBe(false);
    expect(mem2.success).toBe(true);
    expect(mem2.tier).toBe("semantic");
    expect(mem2.title).toBe("merge notes");
    expect(mem2.matched).toBe(1);
    // And the search card still resolves its own result (nothing else was
    // disturbed).
    dispatch({
      agent_id: ID,
      event: {
        kind: "tool_result",
        tool_call_id: "call-s",
        result: { success: true, output: "1 matches in 1 files (engine: walk)" },
      },
    });
    const search = agent(ID).transcript.find((e) => e.kind === "tool" && e.name === "search");
    if (!search || search.kind !== "tool") throw new Error("search tool entry missing");
    expect(search.calls[0].result).not.toBeNull();
  });

  it("tool_call_arg_delta: accumulates args into the in-flight call", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
    });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "tool_call_arg_delta", index: 0, fragment: "{\"cmd\":" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "tool_call_arg_delta", index: 0, fragment: "\"ls\"}" } });
    const tool = agent(ID).transcript.find((e) => e.kind === "tool");
    if (tool && tool.kind === "tool") expect(tool.calls[0].args).toBe('{"cmd":"ls"}');
  });

  it("tool_call_arg_delta: accumulates args into a running memory entry (query-chip source)", () => {
    // Memory entries carry the streamed args so the memory_search card can
    // show WHAT was searched (memorySearchLabel parses them at render
    // time). Matched by the owning call's index while the entry runs.
    // (Memory entries always enter the store now — visibility is a
    // render-time concern, so no flag to set.)
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-m", name: "memory_search" },
    });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "tool_call_arg_delta", index: 0, fragment: '{"query":"resize' } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "tool_call_arg_delta", index: 0, fragment: ' seam"}' } });
    const mem = agent(ID).transcript.find((e) => e.kind === "memory");
    if (!mem || mem.kind !== "memory") throw new Error("memory entry missing");
    expect(mem.args).toBe('{"query":"resize seam"}');
    expect(mem.running).toBe(true);
  });

  it("approval_request: records pending approval with preview", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "approval_request",
        tool_call_id: "call-1",
        tool_name: "file_edit",
        args: { path: "a.ts" },
        preview: { kind: "diff", path: "a.ts", diff: "--- a\n+++ b" },
        core_operation: false,
      },
    });
    const a = agent(ID);
    expect(a.pendingApproval).toEqual({
      toolCallId: "call-1",
      toolName: "file_edit",
      args: { path: "a.ts" },
      preview: { kind: "diff", path: "a.ts", diff: "--- a\n+++ b" },
      coreOperation: false,
    });
  });

  it("user_question: records pending question, cleared on next turn start", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "user_question",
        question_id: "q-1",
        question: "Favorite color?",
        options: [
          { label: "Blue", description: null },
          { label: "Green", description: null },
          { label: "Red", description: null },
        ],
      },
    });
    const a = agent(ID);
    expect(a.pendingQuestion).toEqual({
      questionId: "q-1",
      question: "Favorite color?",
      options: [
        { label: "Blue", description: null },
        { label: "Green", description: null },
        { label: "Red", description: null },
      ],
    });
    // A new turn (Started) clears the pending question — the agent resumed
    // with the answer, so the prompt is no longer pending.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "started" },
    });
    expect(agent(ID).pendingQuestion).toBeNull();
  });

  it("question_answered: appends qa transcript entry + sets answered marker, cleared on started", () => {
    // Dispatch a pending question first.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "user_question",
        question_id: "q-1",
        question: "Favorite color?",
        options: [
          { label: "Blue", description: null },
          { label: "Green", description: null },
        ],
      },
    });
    // Record an answer (as the InputBar / QuestionPrompt would).
    useAgentStore.getState().recordQuestionAnswer(ID, {
      questionId: "q-1",
      question: "Favorite color?",
      answer: "Blue",
    });
    const a = agent(ID);
    // A `qa` transcript entry was appended with the question + answer.
    const qa = a.transcript.find((e) => e.kind === "qa");
    expect(qa).toBeDefined();
    expect(qa).toEqual({
      kind: "qa",
      question: "Favorite color?",
      answer: "Blue",
      ts: expect.any(Number),
      entryId: expect.any(Number),
    });
    // The answered marker is set so the live prompt collapses immediately.
    expect(a.pendingQuestionAnswered).toBe("q-1");
    // The pending question is TERMINATED on answer (ask_user-stuck regression,
    // 2026-08-22): the backend resumes the same turn and never emits a fresh
    // `started` mid-turn, so pendingQuestion must clear HERE or the InflightBar
    // stays on "waiting for your answer…" for the rest of the turn.
    expect(a.pendingQuestion).toBeNull();
    // A new turn (Started) clears the answered marker + freeform mode too
    // (the question was fully resolved).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "started" },
    });
    const after = agent(ID);
    expect(after.pendingQuestion).toBeNull();
    expect(after.pendingQuestionAnswered).toBeNull();
    expect(after.freeformQuestionId).toBeNull();
  });

  it("setFreeformQuestion: enters + exits freeform-answer mode, cleared on started", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "user_question",
        question_id: "q-2",
        question: "Tea or coffee?",
        options: [{ label: "Tea", description: null }],
      },
    });
    // Enter freeform mode (the user picked the "Let's talk about it" number).
    useAgentStore.getState().setFreeformQuestion(ID, "q-2");
    expect(agent(ID).freeformQuestionId).toBe("q-2");
    // Exit (cancel) — keeps the question pending.
    useAgentStore.getState().setFreeformQuestion(ID, null);
    expect(agent(ID).freeformQuestionId).toBeNull();
    expect(agent(ID).pendingQuestion).not.toBeNull();
    // A new turn clears freeform mode too.
    useAgentStore.getState().setFreeformQuestion(ID, "q-2");
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "started" },
    });
    expect(agent(ID).freeformQuestionId).toBeNull();
  });

  it("question_answered: tears down in-flight freeform-answer mode (B1 regression)", () => {
    // A pending question + the user entered freeform mode (picked "Let's talk
    // about it"), then answered via a different path (e.g. clicked an option).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "user_question",
        question_id: "q-3",
        question: "Tea or coffee?",
        options: [{ label: "Tea", description: null }, { label: "Coffee", description: null }],
      },
    });
    useAgentStore.getState().setFreeformQuestion(ID, "q-3");
    expect(agent(ID).freeformQuestionId).toBe("q-3");
    // Now the question is answered (e.g. via the QuestionPrompt button).
    useAgentStore.getState().recordQuestionAnswer(ID, {
      questionId: "q-3",
      question: "Tea or coffee?",
      answer: "Tea",
    });
    // Freeform mode must be torn down so the InputBar doesn't get stuck.
    expect(agent(ID).freeformQuestionId).toBeNull();
    expect(agent(ID).pendingQuestionAnswered).toBe("q-3");
  });

  it("tool_result: updates the call and clears approval", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "approval_request", tool_call_id: "call-1", tool_name: "shell", args: {}, preview: null, core_operation: false },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "call-1", result: { success: true, output: "ok" } },
    });
    const a = agent(ID);
    const tool = a.transcript.find((e) => e.kind === "tool");
    if (tool && tool.kind === "tool") {
      expect(tool.calls[0].result).toEqual({ success: true, output: "ok" });
    }
    expect(a.pendingApproval).toBeNull();
  });

  it("tool_result: captures a lastDiff snapshot for file_edit", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "file_edit" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_arg_delta", index: 0, fragment: '{"path":"a.ts","old_string":"x","new_string":"y"}' },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "approval_request", tool_call_id: "call-1", tool_name: "file_edit", args: {}, preview: null, core_operation: false },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "call-1", result: { success: true, output: "edited" } },
    });
    const ld = useAgentStore.getState().lastDiff;
    expect(ld).not.toBeNull();
    expect(ld?.toolName).toBe("file_edit");
    expect(ld?.path).toBe("a.ts");
    expect(ld?.oldString).toBe("x");
    expect(ld?.newString).toBe("y");
    // The same snapshot feeds the current top plan's changed-file list.
    const planDiffs = useAgentStore.getState().planDiffs;
    expect(planDiffs).toHaveLength(1);
    expect(planDiffs[0]).toMatchObject({ toolName: "file_edit", path: "a.ts" });
  });

  it("tool_result: planDiffs dedupes per path, newest first", () => {
    function edit(path: string, id: string) {
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: { kind: "tool_call_start", index: 0, id, name: "file_edit" },
      });
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: {
          kind: "tool_call_arg_delta",
          index: 0,
          fragment: `{"path":"${path}","old_string":"x","new_string":"y"}`,
        },
      });
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: {
          kind: "tool_result",
          tool_call_id: id,
          result: { success: true, output: "edited" },
        },
      });
    }
    edit("a.ts", "call-1");
    edit("b.ts", "call-2");
    expect(useAgentStore.getState().planDiffs.map((d) => d.path)).toEqual([
      "b.ts",
      "a.ts",
    ]);
    // Re-editing a.ts replaces its earlier entry (newest snapshot wins) and
    // moves it to the front — still one entry per path.
    edit("a.ts", "call-3");
    const planDiffs = useAgentStore.getState().planDiffs;
    expect(planDiffs).toHaveLength(2);
    expect(planDiffs.map((d) => d.path)).toEqual(["a.ts", "b.ts"]);
    expect(planDiffs[0].timestamp).toBeGreaterThanOrEqual(planDiffs[1].timestamp);
  });

  it("regression (review L1): planDiffs caps at MAX_PLAN_DIFFS, keeping the newest paths", () => {
    // MAX_PLAN_DIFFS + 1 distinct edited paths: the oldest (path-0) drops
    // off; the list stays newest-first (the uncapped list grew for the life
    // of the plan, holding full diff bodies).
    const total = MAX_PLAN_DIFFS + 1;
    for (let i = 0; i < total; i++) {
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: { kind: "tool_call_start", index: 0, id: `call-${i}`, name: "file_edit" },
      });
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: {
          kind: "tool_call_arg_delta",
          index: 0,
          fragment: `{"path":"path-${i}","old_string":"x","new_string":"y"}`,
        },
      });
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: {
          kind: "tool_result",
          tool_call_id: `call-${i}`,
          result: { success: true, output: "edited" },
        },
      });
    }
    const planDiffs = useAgentStore.getState().planDiffs;
    expect(planDiffs).toHaveLength(MAX_PLAN_DIFFS);
    // Newest first: the latest edit leads, the oldest (path-0) was dropped.
    expect(planDiffs[0].path).toBe(`path-${total - 1}`);
    expect(planDiffs.map((d) => d.path)).not.toContain("path-0");
  });

  it("usage: accumulates tokens + computes timing", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "usage", prompt_tokens: 100, completion_tokens: 50, reasoning_tokens: 10, cached_tokens: 20, ttft_ms: 1000, generation_ms: 2000 },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "usage", prompt_tokens: 50, completion_tokens: 25, reasoning_tokens: 5, cached_tokens: 10, ttft_ms: null, generation_ms: null },
    });
    const a = agent(ID);
    expect(a.tokenUsage).toEqual({ prompt: 150, completion: 75, reasoning: 15, cached: 30 });
    expect(a.lastRequestTiming).toEqual({
      ttft_ms: null,
      generation_ms: null,
      input_tok_sec: null,
      output_tok_sec: null,
    });
  });

  it("sessionTiming: accumulates across requests, persists across started, resets on clearConversation", () => {
    // First usage event (timed).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "usage", prompt_tokens: 100, completion_tokens: 50, reasoning_tokens: 10, cached_tokens: 20, ttft_ms: 1000, generation_ms: 2000 },
    });
    let a = agent(ID);
    expect(a.sessionTiming).toEqual({
      ttft_ms_total: 1000,
      generation_ms_total: 2000,
      timed_requests: 1,
      prompt_tokens: 100,
      completion_tokens: 50,
      recentTiming: [{ completion_tokens: 50, generation_ms: 2000 }],
    });
    expect(a.lastContextBreakdown).toEqual({
      prompt: 100,
      completion: 50,
      reasoning: 10,
      cached: 20,
    });

    // A new turn (started) must NOT reset sessionTiming (it persists across
    // turns — only clearConversation resets it). The per-turn tokenUsage
    // resets to 0, but sessionTiming accumulates.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    a = agent(ID);
    expect(a.tokenUsage).toEqual({ prompt: 0, completion: 0, reasoning: 0, cached: 0 });
    expect(a.sessionTiming.timed_requests).toBe(1);
    expect(a.sessionTiming.prompt_tokens).toBe(100);

    // Second usage in the new turn — sessionTiming accumulates.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "usage", prompt_tokens: 200, completion_tokens: 100, reasoning_tokens: 0, cached_tokens: 0, ttft_ms: 500, generation_ms: 1000 },
    });
    a = agent(ID);
    expect(a.sessionTiming.timed_requests).toBe(2);
    expect(a.sessionTiming.ttft_ms_total).toBe(1500);
    expect(a.sessionTiming.prompt_tokens).toBe(300);
    // Aggregate input rate = total prompt / total TTFT = 300 / (1500/1000) =
    // 300 / 1.5 = 200. (The old buggy formula divided by the *average*
    // per-request time, yielding 300 / 0.75 = 400 — inflated by the request
    // count. aggregateTokPerSec uses total time, the correct throughput.)
    const inputRate = aggregateTokPerSec(
      a.sessionTiming.prompt_tokens,
      a.sessionTiming.ttft_ms_total,
    );
    expect(inputRate).toBeCloseTo(200);

    // clearConversation resets sessionTiming (fresh session).
    useAgentStore.getState().clearConversation(ID);
    a = agent(ID);
    expect(a.sessionTiming).toEqual({
      ttft_ms_total: 0,
      generation_ms_total: 0,
      timed_requests: 0,
      prompt_tokens: 0,
      completion_tokens: 0,
      recentTiming: [],
    });
    expect(a.lastContextBreakdown).toBeNull();
  });

  it("recentTiming: rolling last-3 window feeds the status-bar tok/sec", () => {
    // Fresh state: empty window → no rate (the bar shows '—').
    expect(emptyAgentState().sessionTiming.recentTiming).toEqual([]);
    expect(recentOutputTokPerSec(emptyAgentState().sessionTiming)).toBeNull();

    // Four timed requests with distinct generation timings — the window must
    // hold only the LAST 3 (oldest dropped), newest last.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "usage", prompt_tokens: 0, completion_tokens: 100, reasoning_tokens: 0, cached_tokens: 0, ttft_ms: 100, generation_ms: 1000 },
    }); // dropped
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "usage", prompt_tokens: 0, completion_tokens: 100, reasoning_tokens: 0, cached_tokens: 0, ttft_ms: 100, generation_ms: 3000 },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "usage", prompt_tokens: 0, completion_tokens: 100, reasoning_tokens: 0, cached_tokens: 0, ttft_ms: 100, generation_ms: 6000 },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "usage", prompt_tokens: 0, completion_tokens: 100, reasoning_tokens: 0, cached_tokens: 0, ttft_ms: 100, generation_ms: 2000 },
    });
    const a = agent(ID);
    expect(a.sessionTiming.recentTiming).toEqual([
      { completion_tokens: 100, generation_ms: 3000 },
      { completion_tokens: 100, generation_ms: 6000 },
      { completion_tokens: 100, generation_ms: 2000 },
    ]);
    // Status-bar rate = total-over-total across the window (300 tok / 11 s ≈
    // 27.3 tok/s) — NOT the average of the per-request rates (which would be
    // (33.3 + 16.7 + 50) / 3 ≈ 33.3 — the N× inflation lesson from 463ddd4).
    expect(recentOutputTokPerSec(a.sessionTiming)).toBeCloseTo(300 / 11);

    // A request that reported no generation time doesn't enter the window
    // (tokens with no time denominator would inflate the rate).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "usage", prompt_tokens: 0, completion_tokens: 999, reasoning_tokens: 0, cached_tokens: 0, ttft_ms: 100, generation_ms: null },
    });
    expect(agent(ID).sessionTiming.recentTiming).toHaveLength(3);
    expect(
      agent(ID).sessionTiming.recentTiming.map((s) => s.completion_tokens),
    ).toEqual([100, 100, 100]);
  });

  it("context_usage: records the latest context-window fill", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "context_usage", used: 1000, max: 8000, breakdown: { system: 500, user: 200, assistant: 200, tool: 100 } } });
    expect(agent(ID).contextUsage).toEqual({ used: 1000, max: 8000 });
    expect(agent(ID).contextBreakdown).toEqual({ system: 500, user: 200, assistant: 200, tool: 100 });
  });

  it("compacted: appends a transcript confirmation and preserves running", () => {
    // Seed the agent as running, then feed a compacted event. The reducer
    // must append the before → after confirmation without touching `running`.
    useAgentStore.setState((s) => ({
      agents: { ...s.agents, [ID]: { ...emptyAgentState(), running: true } },
    }));
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "compacted", before: 500_000, after: 24_000 } });
    const a = agent(ID);
    const note = a.transcript.find((e) => e.kind === "assistant");
    expect(note && note.kind === "assistant" ? note.text : "").toContain("Context compacted");
    expect(note && note.kind === "assistant" ? note.text : "").toContain("500K");
    expect(note && note.kind === "assistant" ? note.text : "").toContain("24K");
    expect(a.running).toBe(true);
  });

  it("compact_started: appends a transcript announcement and preserves running", () => {
    // The backend announces compaction start on every path (slash command,
    // popup button, mid-turn, auto) — the reducer appends the "Compacting
    // context…" entry without touching `running`.
    useAgentStore.setState((s) => ({
      agents: { ...s.agents, [ID]: { ...emptyAgentState(), running: true } },
    }));
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "compact_started" } });
    const a = agent(ID);
    const note = a.transcript.find((e) => e.kind === "assistant");
    expect(note && note.kind === "assistant" ? note.text : "").toContain("Compacting context");
    expect(a.running).toBe(true);
  });

  it("compacted with no reduction: appends a nothing-to-compact note", () => {
    // The backend emits `compacted` whenever the summarization completed,
    // even without a reduction, so every compact_started has a paired end.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "compacted", before: 8_000, after: 8_000 } });
    const a = agent(ID);
    const note = a.transcript.find((e) => e.kind === "assistant");
    expect(note && note.kind === "assistant" ? note.text : "").toContain("Nothing to compact");
  });

  it("workflow_state_changed: updates per-agent workflowStates + bumps planVersion", () => {
    const before = useAgentStore.getState().planVersion;
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "workflow_state_changed", state: "executing", top_plan_id: "p1" } });
    const s = useAgentStore.getState();
    expect(s.workflowStates[ID]).toBe("executing");
    expect(s.planVersion).toBe(before + 1);
    // The root plan id is recorded (no agents registered → no main-agent
    // reset fires, so topPlanId takes the event's value).
    expect(s.topPlanId).toBe("p1");
  });

  it("workflow_state_changed: resets planDiffs/lastDiff when the top plan id changes (main agent)", () => {
    // Register the main agent so the reset (main-agent-only) can fire.
    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null },
    ]);
    useAgentStore.getState().selectDiffPath("a.ts");
    // Seed a captured diff via a completed file_edit.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "file_edit" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_arg_delta", index: 0, fragment: '{"path":"a.ts","old_string":"x","new_string":"y"}' },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "call-1", result: { success: true, output: "edited" } },
    });
    expect(useAgentStore.getState().planDiffs).toHaveLength(1);
    expect(useAgentStore.getState().lastDiff).not.toBeNull();

    // A new top-level plan (root id null → "p1"): the list resets.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "workflow_state_changed", state: "executing", top_plan_id: "p1" },
    });
    let s = useAgentStore.getState();
    expect(s.topPlanId).toBe("p1");
    expect(s.planDiffs).toEqual([]);
    expect(s.lastDiff).toBeNull();
    expect(s.selectedDiffPath).toBeNull();

    // Same root id (e.g. a skill transition or sub-plan activity): no reset.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-2", name: "file_edit" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_arg_delta", index: 0, fragment: '{"path":"b.ts","old_string":"x","new_string":"y"}' },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "call-2", result: { success: true, output: "edited" } },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "workflow_state_changed", state: "skill", top_plan_id: "p1" },
    });
    s = useAgentStore.getState();
    expect(s.planDiffs.map((d) => d.path)).toEqual(["b.ts"]);
    expect(s.lastDiff).not.toBeNull();

    // A different root id (a fresh top-level plan): reset again.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "workflow_state_changed", state: "executing", top_plan_id: "p2" },
    });
    s = useAgentStore.getState();
    expect(s.topPlanId).toBe("p2");
    expect(s.planDiffs).toEqual([]);
    expect(s.lastDiff).toBeNull();
  });

  it("workflow_state_changed: a subagent event does not reset planDiffs", () => {
    // Main agent + one subagent. The reset only fires for the main agent.
    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null },
      { id: SUB, name: "sub", running: true, parent_id: ID },
    ]);
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "workflow_state_changed", state: "executing", top_plan_id: "p1" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "file_edit" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_arg_delta", index: 0, fragment: '{"path":"a.ts","old_string":"x","new_string":"y"}' },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "call-1", result: { success: true, output: "edited" } },
    });
    expect(useAgentStore.getState().planDiffs).toHaveLength(1);

    // The subagent's workflow_state_changed carries a different root id — it
    // must NOT reset the main plan's changed-file list (the tracker follows
    // the id, but only the main agent's events reset the list).
    useAgentStore.getState().handleAgentEvent({
      agent_id: SUB,
      event: { kind: "workflow_state_changed", state: "executing", top_plan_id: "sub-root" },
    });
    const s = useAgentStore.getState();
    expect(s.topPlanId).toBe("sub-root");
    expect(s.planDiffs.map((d) => d.path)).toEqual(["a.ts"]);
    expect(s.lastDiff).not.toBeNull();
  });

  it("selectDiffPath: round-trips the dropdown pick", () => {
    expect(useAgentStore.getState().selectedDiffPath).toBeNull();
    useAgentStore.getState().selectDiffPath("a.ts");
    expect(useAgentStore.getState().selectedDiffPath).toBe("a.ts");
    useAgentStore.getState().selectDiffPath(null);
    expect(useAgentStore.getState().selectedDiffPath).toBeNull();
  });

  it("step_completed: bumps planVersion", () => {
    const before = useAgentStore.getState().planVersion;
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "step_completed", step_index: 1 } });
    expect(useAgentStore.getState().planVersion).toBe(before + 1);
  });

  it("suggestion_injected: marks oldest pending steer landed + schedules removal", () => {
    useAgentStore.getState().addSteer(ID, "first");
    useAgentStore.getState().addSteer(ID, "second");
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "suggestion_injected", text: "injected", images: [] } });
    const a = agent(ID);
    expect(a.steers[0].status).toBe("landed");
    expect(a.steers[1].status).toBe("pending");
    // The steer transcript entry is pushed.
    expect(a.transcript.some((e) => e.kind === "steer" && e.text === "injected")).toBe(true);
  });

  it("suggestion_injected: carries the steer's images into the transcript entry", () => {
    // REGRESSION (steered images dropped, 2027-01-07): the injected steer's
    // images must ride the event into the transcript entry (rendered as
    // thumbnails like a user prompt's), and the pending bubble keeps them
    // for the image indicator.
    const img = "data:image/png;base64,iVBORw0KGgo=";
    useAgentStore.getState().addSteer(ID, "look at this", [img]);
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "suggestion_injected", text: "look at this", images: [img] },
    });
    const a = agent(ID);
    // The bubble entry carries the images (image indicator).
    expect(a.steers[0].images).toEqual([img]);
    // The transcript steer entry carries the images.
    const entry = a.transcript.find((e) => e.kind === "steer" && e.text === "look at this");
    expect(entry).toBeDefined();
    if (entry?.kind === "steer") {
      expect(entry.images).toEqual([img]);
    }
  });

  it("prompt_dispatched: appends a user transcript entry, flushing streaming text", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "partial" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "prompt_dispatched", text: "goal", images: [] } });
    const a = agent(ID);
    expect(a.streamingText).toBe("");
    expect(a.transcript[0]).toEqual({ kind: "assistant", text: "partial", ts: expect.any(Number), entryId: expect.any(Number) });
    expect(a.transcript[1]).toEqual({ kind: "user", text: "goal", ts: expect.any(Number), entryId: expect.any(Number) });
  });

  it("skill_started: pushes a skill entry, flushing streaming text above it", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "thinking…" } });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "skill_started", name: "merge_to_main", prompt: "Merge the branch into main." },
    });
    const a = agent(ID);
    expect(a.streamingText).toBe("");
    const skillIdx = a.transcript.findIndex((e) => e.kind === "skill");
    const asstIdx = a.transcript.findIndex((e) => e.kind === "assistant" && e.text === "thinking…");
    expect(skillIdx).toBeGreaterThan(-1);
    expect(asstIdx).toBeLessThan(skillIdx);
    const skill = a.transcript[skillIdx];
    if (skill.kind === "skill") {
      expect(skill.name).toBe("merge_to_main");
      expect(skill.prompt).toBe("Merge the branch into main.");
    }
  });

  it("model_changed: updates agentModels for the agent", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "model_changed", model: "claude-4" },
    });
    expect(useAgentStore.getState().agentModels[ID]).toBe("claude-4");
    // Absent provider clears the agent's entry — the UI falls back to
    // resolving the model id against endpoints.toml (pre-wire behavior).
    expect(useAgentStore.getState().agentProviders[ID]).toBeUndefined();
  });

  it("model_changed: stamps the serving endpoint into agentProviders (collision-safe label)", () => {
    // Backlog 2980ca67: when the same model id is listed under two
    // endpoints, the backend must NAME the serving endpoint — the status bar
    // labels the provider from this wire value, never first-match over the
    // endpoint list. An explicit provider always upserts; a later
    // provider-less event CLEARS the entry (fall back to model-id
    // resolution), mirroring model-presence semantics.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "model_changed", model: "glm-4.7", provider: "zai" },
    });
    expect(useAgentStore.getState().agentProviders[ID]).toBe("zai");
    expect(useAgentStore.getState().agentModels[ID]).toBe("glm-4.7");

    // A provider-less change clears the stale name (never labels the OLD
    // endpoint against the NEW model).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "model_changed", model: "gpt-5" },
    });
    expect(useAgentStore.getState().agentProviders[ID]).toBeUndefined();

    // Empty string is treated as absent (mock-backed loops send "").
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "model_changed", model: "mock-model", provider: "" },
    });
    expect(useAgentStore.getState().agentProviders[ID]).toBeUndefined();
  });

  it("model_changed for one agent does not change another agent's model", () => {
    // Invariant pin for the per-agent picker (backlog #83, 2026-08-22): a
    // ModelChanged event must stamp ONLY that agent's agentModels entry,
    // never a sibling's. The reducer was already per-agent pre-fix; the
    // defect lived in the backend's global fan-out + StatusBar global-store
    // writes, whose fail-pre-fix regression test is
    // swap_provider_into_loop_swaps_only_the_target_agent (config_io.rs).
    const OTHER = (ID + 1) as AgentId;
    useAgentStore.setState({ agentModels: { [ID]: "gpt-4o", [OTHER]: "grok" } });
    useAgentStore.getState().handleAgentEvent({
      agent_id: OTHER,
      event: { kind: "model_changed", model: "claude-4" },
    });
    const models = useAgentStore.getState().agentModels;
    expect(models[OTHER]).toBe("claude-4");
    expect(models[ID]).toBe("gpt-4o");
  });

  it("model_changed: records the wire-reported reasoning effort for the agent", () => {
    // REGRESSION (status-bar effort stale for auto-selected models,
    // 2027-01-07, backlog 51dab4da): the backend reports the effective
    // reasoning effort of the model serving the current context (per-context
    // ModelRef.reasoning_effort override, else the model's default chain
    // ModelSpec.reasoning_effort → endpoint default → "max") alongside the
    // model — the status bar displays THIS wire value, never the toolbar
    // echo or the endpoint-level default. Pre-fix the event carried no
    // effort and the bar showed a stale/wrong value whenever the model was
    // auto-selected / resolved per-context.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "model_changed",
        model: "glm-flash",
        provider: "ep2",
        reasoning_effort: "low",
      },
    });
    expect(useAgentStore.getState().agentWireEfforts[ID]).toBe("low");
    expect(useAgentStore.getState().agentModels[ID]).toBe("glm-flash");

    // An effort-less change CLEARS the stale value (never label the OLD
    // effort against the NEW model) — mirroring agentProviders semantics;
    // the UI falls back to the toolbar echo / endpoint default.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "model_changed", model: "mock-model" },
    });
    expect(useAgentStore.getState().agentWireEfforts[ID]).toBeUndefined();
  });

  it("setAgentEffort records per-agent effort; clearAgentEfforts resets all", () => {
    // Regression (review L2, 2026-08-22): effort is now per-agent (each
    // agent's provider is rebuilt with its own effort), so the toolbar must
    // be able to record/read each agent's effort instead of one global value.
    const OTHER = (ID + 1) as AgentId;
    const st = useAgentStore.getState();
    st.setAgentEffort(ID, "high");
    st.setAgentEffort(OTHER, "low");
    expect(useAgentStore.getState().agentEfforts).toEqual({
      [ID]: "high",
      [OTHER]: "low",
    });
    useAgentStore.getState().clearAgentEfforts();
    expect(useAgentStore.getState().agentEfforts).toEqual({});
  });

  it("exited: drops the agent's per-agent effort record", () => {
    const OTHER = (ID + 1) as AgentId;
    useAgentStore.setState({
      agentEfforts: { [ID]: "high", [OTHER]: "low" },
      agents: {
        [ID]: emptyAgentState(),
        [OTHER]: emptyAgentState(),
      },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: OTHER,
      event: { kind: "exited" },
    });
    const efforts = useAgentStore.getState().agentEfforts;
    expect(efforts[OTHER]).toBeUndefined();
    expect(efforts[ID]).toBe("high");
  });

  it("memory_recalled: pushes an auto-recall memory entry (always — rendering hides it when off)", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "memory_recalled",
        hits: [
          { tier: "semantic", title: "backlog decisions" },
          { tier: "procedural", title: "release checklist" },
        ],
      },
    });
    const entry = agent(ID).transcript.find((e) => e.kind === "memory");
    expect(entry).toBeDefined();
    if (entry?.kind === "memory") {
      expect(entry.name).toBe("auto-recall");
      expect(entry.snippet).toContain("2 hit(s)");
      expect(entry.snippet).toContain("backlog decisions");
      expect(entry.hits).toHaveLength(2);
      expect(entry.hits?.[1]).toEqual({ tier: "procedural", title: "release checklist" });
      expect(entry.running).toBe(false);
    }
  });

  it("memory_recalled: the entry enters the store even with showToolActivity off (render-time hiding)", () => {
    // Backlog db489070: visibility moved from the reducer to the render
    // layer — the store keeps every entry either way (the model's context
    // echo is built server-side and is unaffected); Conversation.tsx +
    // isActivityEntry hide the card when the toggle is off.
    useAgentStore.setState({ showToolActivity: false });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "memory_recalled",
        hits: [{ tier: "semantic", title: "x" }],
      },
    });
    expect(agent(ID).transcript.some((e) => e.kind === "memory")).toBe(true);
  });

  it("vision_describe + vision_described: push and complete an image-parsing card", () => {
    // The announcement (before the vision round-trip) pushes a running
    // entry; the paired described event fills it in — the card then holds
    // the query sent to the vision model and the response it returned.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "vision_describe",
        index: 1,
        total: 1,
        query: "Describe this image in detail.",
      },
    });
    const running = agent(ID).transcript.find((e) => e.kind === "vision");
    expect(running).toBeDefined();
    if (running?.kind === "vision") {
      expect(running.index).toBe(1);
      expect(running.total).toBe(1);
      expect(running.query).toBe("Describe this image in detail.");
      expect(running.description).toBeNull();
      expect(running.running).toBe(true);
    }
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "vision_described",
        index: 1,
        total: 1,
        success: true,
        description: "a photo of a cat",
      },
    });
    const done = agent(ID).transcript.find((e) => e.kind === "vision");
    expect(done).toBeDefined();
    if (done?.kind === "vision") {
      expect(done.description).toBe("a photo of a cat");
      expect(done.success).toBe(true);
      expect(done.running).toBe(false);
    }
    // One card per image — no duplicates after the pair.
    expect(agent(ID).transcript.filter((e) => e.kind === "vision")).toHaveLength(1);
  });

  it("vision_described failure: carries the error text with success=false", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "vision_describe", index: 1, total: 1, query: "q" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "vision_described",
        index: 1,
        total: 1,
        success: false,
        description: "vision is down",
      },
    });
    const entry = agent(ID).transcript.find((e) => e.kind === "vision");
    expect(entry).toBeDefined();
    if (entry?.kind === "vision") {
      expect(entry.success).toBe(false);
      expect(entry.description).toBe("vision is down");
      expect(entry.running).toBe(false);
    }
  });

  it("vision_describe: a re-announced index replaces its running entry (no duplicate)", () => {
    // Provider retry: the same image is announced twice before any result —
    // the second announce must replace the first, not stack a second card.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "vision_describe", index: 1, total: 1, query: "q" },
    });
    const id = agent(ID).transcript.find((e) => e.kind === "vision")!.entryId;
    expect(id).toBeDefined();
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "vision_describe", index: 1, total: 1, query: "q" },
    });
    const cards = agent(ID).transcript.filter((e) => e.kind === "vision");
    expect(cards).toHaveLength(1);
    // The replacement spreads the previous entry, so the card's entryId —
    // its React key — survives the retry (review LOW 1, plan 225e0dad: a
    // fresh literal would get a new id from the identity pass and the
    // card's row state would reset mid-stream).
    expect(cards[0]!.entryId).toBe(id);
  });

  it("vision events: multi-image prompts get one card per image, keyed by index", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "vision_describe", index: 1, total: 2, query: "q1" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "vision_describe", index: 2, total: 2, query: "q2" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "vision_described", index: 2, total: 2, success: true, description: "d2" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "vision_described", index: 1, total: 2, success: true, description: "d1" },
    });
    const cards = agent(ID).transcript.filter((e) => e.kind === "vision");
    expect(cards).toHaveLength(2);
    if (cards[0]?.kind === "vision" && cards[1]?.kind === "vision") {
      // Completion order does not matter — each card carries its own answer.
      expect(cards[0].index).toBe(1);
      expect(cards[0].description).toBe("d1");
      expect(cards[1].index).toBe(2);
      expect(cards[1].description).toBe("d2");
      expect(cards.every((c) => c.kind === "vision" && !c.running)).toBe(true);
    }
  });

  it("vision: a FINAL error finalizes a dangling running card (no spinning forever)", () => {
    // Review LOW 1 (2026-08-27): if the agent task dies mid-vision-call
    // (panic between VisionDescribe and VisionDescribed), the terminal error
    // event is the last signal the UI ever gets — a running "image parsing"
    // card must be finalized there, not left spinning.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "vision_describe", index: 1, total: 1, query: "q" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "error", error: "task aborted", retrying: false },
    });
    const entry = agent(ID).transcript.find((e) => e.kind === "vision");
    expect(entry).toBeDefined();
    if (entry?.kind === "vision") {
      expect(entry.running).toBe(false);
      expect(entry.success).toBe(false);
      expect(entry.description).toBe("(interrupted)");
    }
  });

  it("vision: a retrying error leaves a running card alone (the call is still alive)", () => {
    // A provider retry surfaces retrying:true errors WITHOUT ending the
    // turn — the vision call may still be in flight, so the card must keep
    // spinning for its paired VisionDescribed.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "vision_describe", index: 1, total: 1, query: "q" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "error", error: "attempt 1/3 failed", retrying: true },
    });
    const entry = agent(ID).transcript.find((e) => e.kind === "vision");
    expect(entry).toBeDefined();
    if (entry?.kind === "vision") {
      expect(entry.running).toBe(true);
      expect(entry.description).toBeNull();
    }
  });

  it("regression (backlog 63cbc20f): finished sweeps a still-running tool card (steer cut the stream after tool_call_start)", () => {
    // A steer landing mid-stream ends the turn at the stream break point —
    // BEFORE the tool loop, so the announced read_files call never gets a
    // ToolResult and its card stayed "running" forever (the stuck-card bug).
    // The terminal Finished event is the last signal the UI gets: sweep every
    // still-running card with a failed "(interrupted)" placeholder — the
    // same pairing precedent reduceError already applies to vision cards.
    const dispatch = useAgentStore.getState().handleAgentEvent;
    dispatch({ agent_id: ID, event: { kind: "tool_call_start", index: 0, id: "c1", name: "read_files" } });
    dispatch({ agent_id: ID, event: { kind: "tool_call_arg_delta", index: 0, fragment: "{}" } });
    dispatch({ agent_id: ID, event: { kind: "suggestion_injected", text: "do this instead", images: [] } });
    dispatch({ agent_id: ID, event: { kind: "finished", reason: "stop" } });
    const entry = agent(ID).transcript.find((e) => e.kind === "tool" && e.name === "read_files");
    if (!entry || entry.kind !== "tool") throw new Error("read_files tool entry missing");
    expect(entry.calls).toHaveLength(1);
    expect(entry.calls[0].result).toEqual({ success: false, output: "(interrupted)" });
  });

  it("regression (backlog 63cbc20f): a late real tool_result replaces the swept placeholder", () => {
    // The sweep is a placeholder, not a verdict: if the real result lands
    // after the terminal event (reordering / delayed dispatch), it must
    // still win — reduceToolResult matches by tool_call_id and overwrites.
    const dispatch = useAgentStore.getState().handleAgentEvent;
    dispatch({ agent_id: ID, event: { kind: "tool_call_start", index: 0, id: "c1", name: "read_files" } });
    dispatch({ agent_id: ID, event: { kind: "finished", reason: "stop" } });
    dispatch({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "c1", result: { success: true, output: "ok" } },
    });
    const entry = agent(ID).transcript.find((e) => e.kind === "tool");
    if (!entry || entry.kind !== "tool") throw new Error("tool entry missing");
    expect(entry.calls[0].result).toEqual({ success: true, output: "ok" });
  });

  it("regression (backlog 63cbc20f): a final error sweeps running tool cards; a retrying error does not", () => {
    // A FINAL error ends the turn — an announced call can never complete, so
    // its card must end here. A retrying error keeps the turn (and the call)
    // alive — the card must keep spinning.
    const dispatch = useAgentStore.getState().handleAgentEvent;
    dispatch({ agent_id: ID, event: { kind: "tool_call_start", index: 0, id: "e1", name: "read_files" } });
    dispatch({ agent_id: ID, event: { kind: "error", error: "task aborted", retrying: false } });
    const swept = agent(ID).transcript.find((e) => e.kind === "tool");
    if (!swept || swept.kind !== "tool") throw new Error("tool entry missing");
    expect(swept.calls[0].result).toEqual({ success: false, output: "(interrupted)" });
    // Retry path: fresh store, same start, retrying error → card stays running.
    resetStore();
    dispatch({ agent_id: ID, event: { kind: "tool_call_start", index: 0, id: "e2", name: "read_files" } });
    dispatch({ agent_id: ID, event: { kind: "error", error: "attempt 1/3 failed", retrying: true } });
    const alive = agent(ID).transcript.find((e) => e.kind === "tool");
    if (!alive || alive.kind !== "tool") throw new Error("tool entry missing");
    expect(alive.calls[0].result).toBeNull();
  });

  it("regression (backlog 63cbc20f): finished finalizes a still-running memory entry", () => {
    // Memory entries render as activity cards, not ToolCards, but the same
    // orphaning applies: a steer cutting the stream after the memory tool was
    // announced leaves the entry running forever. The sweep must finalize
    // memory entries too. (Memory entries always enter the store now —
    // visibility is a render-time concern, so no flag to set.)
    const dispatch = useAgentStore.getState().handleAgentEvent;
    dispatch({ agent_id: ID, event: { kind: "tool_call_start", index: 0, id: "call-m2", name: "memory_search" } });
    dispatch({ agent_id: ID, event: { kind: "finished", reason: "stop" } });
    const mem = agent(ID).transcript.find((e) => e.kind === "memory");
    if (!mem || mem.kind !== "memory") throw new Error("memory entry missing");
    expect(mem.running).toBe(false);
    expect(mem.success).toBe(false);
  });

  it("regression (2026-12-30 interrupted turns): interrupt mid-tool-call-stream — terminal card, user follow-up contains only the user's text", () => {
    // Instance 4 of the 2026-12-30 bug: a lone read_files call interrupted
    // mid-stream, then the call text appeared to echo into the user's
    // follow-up message. The root cause was model-side — the cancelled call
    // was never recorded in history, so the raw echo carried dangling
    // tool_calls (fixed on the backend, plan edfff8d9 steps 3-4). This test
    // pins the frontend contract the fix relies on: the interrupted card
    // ends terminal (backend synthetic result, sweep fallback), the flushed
    // assistant text is its own entry, and the user's follow-up entry
    // contains ONLY the user's text.
    const dispatch = useAgentStore.getState().handleAgentEvent;
    dispatch({ agent_id: ID, event: { kind: "text_delta", text: "let me check" } });
    dispatch({ agent_id: ID, event: { kind: "tool_call_start", index: 0, id: "c1", name: "read_files" } });
    dispatch({ agent_id: ID, event: { kind: "tool_call_arg_delta", index: 0, fragment: "{\"files\": [\"report.md\"]}" } });
    // The backend's hard-stop path (plan edfff8d9 step 4) sends the
    // synthetic result BEFORE Finished; the sweep is only a fallback.
    dispatch({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "c1", result: { success: false, output: "interrupted: not run (turn stopped)" } },
    });
    dispatch({ agent_id: ID, event: { kind: "finished", reason: "stop" } });
    // The user's follow-up (pushPrompt → prompt_dispatched).
    dispatch({ agent_id: ID, event: { kind: "prompt_dispatched", text: "follow-up question", images: [] } });

    const t = agent(ID).transcript;
    // The flushed assistant text is its own entry — never merged into the
    // user's message.
    const assistant = t.find((e) => e.kind === "assistant");
    expect(assistant).toEqual({ kind: "assistant", text: "let me check", ts: expect.any(Number), entryId: expect.any(Number) });
    // The interrupted card is terminal with the backend's synthetic result
    // (not spinning, not the sweep placeholder — the real event won).
    const tool = t.find((e) => e.kind === "tool" && e.name === "read_files");
    if (!tool || tool.kind !== "tool") throw new Error("tool entry missing");
    expect(tool.calls[0].result).toEqual({ success: false, output: "interrupted: not run (turn stopped)" });
    // The user's follow-up contains ONLY the user's text — no tool-call
    // text echo — and it is the last entry (correct ordering).
    expect(t[t.length - 1]).toEqual({ kind: "user", text: "follow-up question", ts: expect.any(Number), entryId: expect.any(Number) });
  });

  it("finished: sets running=false, flushes streaming text, and bumps planVersion", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "hello" } });
    const before = useAgentStore.getState().planVersion;
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "finished", reason: "stop" } });
    const a = agent(ID);
    expect(a.running).toBe(false);
    expect(a.streamingText).toBe("");
    expect(a.transcript.some((e) => e.kind === "assistant" && e.text === "hello")).toBe(true);
    // Bumping planVersion on finished makes StatusBar's refreshPlan() re-fetch
    // the true backend workflow state when the agent goes idle — correcting any
    // stale workflowStates value left by a workflow mutation that didn't emit
    // WorkflowStateChanged (e.g. harness complete_step outside run_turn).
    // Without this, the "Merge to main" button (gated on complete/planning)
    // can stay hidden after the last step completes.
    expect(useAgentStore.getState().planVersion).toBe(before + 1);
  });

  it("exited: removes the agent from all maps + falls back to main", () => {
    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null, model: "gpt-5" },
      { id: SUB, name: "sub", running: true, parent_id: ID, model: "claude" },
    ]);
    useAgentStore.getState().setActiveAgent(SUB);
    useAgentStore.getState().setWorkflowState(SUB, "executing");
    useAgentStore.getState().handleAgentEvent({ agent_id: SUB, event: { kind: "exited" } });
    const s = useAgentStore.getState();
    expect(s.agents[SUB]).toBeUndefined();
    expect(s.agentNames[SUB]).toBeUndefined();
    expect(s.agentParents[SUB]).toBeUndefined();
    expect(s.agentModels[SUB]).toBeUndefined();
    expect(s.agentProviders[SUB]).toBeUndefined();
    expect(s.workflowStates[SUB]).toBeUndefined();
    expect(s.activeAgent).toBe(ID);
  });

  it("registerAgents: populates agentModels from info.model (absent model keeps prior)", () => {
    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null, model: "gpt-5" },
      { id: SUB, name: "sub", running: false, parent_id: ID, model: "claude-sonnet" },
    ]);
    const s = useAgentStore.getState();
    expect(s.agentModels[ID]).toBe("gpt-5");
    expect(s.agentModels[SUB]).toBe("claude-sonnet");

    // A subsequent registerAgents with an absent model must NOT clobber a
    // previously-known model (the agent may have exited and lost its loop).
    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null },
    ]);
    expect(useAgentStore.getState().agentModels[ID]).toBe("gpt-5");

    // A present model always wins (live provider swap).
    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null, model: "gpt-5-mini" },
    ]);
    expect(useAgentStore.getState().agentModels[ID]).toBe("gpt-5-mini");
  });

  it("registerAgents: populates agentProviders from info.provider (absent keeps prior)", () => {
    // The wire-carried serving endpoint (backlog 2980ca67): a reported name
    // always wins; an absent one (mock-backed loop) never clobbers a
    // previously-known name — presence-wins, same as agentModels.
    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null, model: "glm-4.7", provider: "zai" },
      { id: SUB, name: "sub", running: false, parent_id: ID, model: "gpt-5" },
    ]);
    const s = useAgentStore.getState();
    expect(s.agentProviders[ID]).toBe("zai");
    expect(s.agentProviders[SUB]).toBeUndefined();

    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null },
    ]);
    expect(useAgentStore.getState().agentProviders[ID]).toBe("zai");

    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null, model: "gpt-5", provider: "openai" },
    ]);
    expect(useAgentStore.getState().agentProviders[ID]).toBe("openai");
  });

  it("seedContextCaps: sets the ctx-bar max, preserves used, ignores unknown ids", () => {
    useAgentStore.getState().registerAgents([
      { id: ID, name: "main", running: true, parent_id: null, model: "gpt-5" },
    ]);
    // Fresh agent: max arrives while used is 0.
    useAgentStore.getState().seedContextCaps([[ID, 128_000]]);
    expect(agent(ID).contextUsage).toEqual({ used: 0, max: 128_000 });

    // A live turn reports a used count via the event; a late seed (e.g. a
    // model-swap re-sync) must update max WITHOUT clobbering used.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "context_usage",
        used: 5_000,
        max: 128_000,
        breakdown: { system: 0, user: 0, assistant: 0, tool: 0 },
      },
    });
    useAgentStore.getState().seedContextCaps([[ID, 256_000]]);
    expect(agent(ID).contextUsage).toEqual({ used: 5_000, max: 256_000 });

    // Unknown ids (exited between listAgents and the seed) are skipped.
    useAgentStore.getState().seedContextCaps([[999, 64_000]]);
    expect(useAgentStore.getState().agents[999]).toBeUndefined();
  });

  it("error (final): adds error entry, stops the agent, and bumps planVersion", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    const before = useAgentStore.getState().planVersion;
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "error", error: "boom", retrying: false } });
    const a = agent(ID);
    expect(a.transcript.some((e) => e.kind === "error" && e.text === "boom")).toBe(true);
    expect(a.running).toBe(false);
    // A final error ends the turn (agent idle) — bump planVersion so StatusBar
    // re-fetches the true backend workflow state, mirroring `finished`.
    expect(useAgentStore.getState().planVersion).toBe(before + 1);
  });

  it("failed flag: set by a FINAL error, cleared by started/finished/clear (Continue button)", () => {
    // Final error → failed=true (Continue button shows: idle + failed).
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    expect(agent(ID).failed).toBe(false);
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "error", error: "boom", retrying: false } });
    expect(agent(ID).failed).toBe(true);
    expect(agent(ID).running).toBe(false);

    // A fresh started (retry / continuation prompt) clears it.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    expect(agent(ID).failed).toBe(false);

    // A clean finished clears it.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "error", error: "boom", retrying: false } });
    expect(agent(ID).failed).toBe(true);
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "finished", reason: "stop" } });
    expect(agent(ID).failed).toBe(false);

    // A retrying error is transient — never sets the failed flag, and keeps
    // the agent running (the retry happens mid-turn, while running is true).
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "error", error: "transient", retrying: true } });
    expect(agent(ID).failed).toBe(false);
    expect(agent(ID).running).toBe(true);

    // Clearing the conversation resets the failure state too.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "error", error: "boom", retrying: false } });
    expect(agent(ID).failed).toBe(true);
    useAgentStore.getState().clearConversation(ID);
    expect(agent(ID).failed).toBe(false);
  });

  it("error (retrying): keeps the agent running and does NOT bump planVersion", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    const before = useAgentStore.getState().planVersion;
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "error", error: "transient", retrying: true } });
    expect(agent(ID).running).toBe(true);
    // A retrying error is transient — the agent is still running, so no idle
    // re-fetch is needed.
    expect(useAgentStore.getState().planVersion).toBe(before);
  });

  // ── Notification-sound effects (complete ding / input ping / doom) ────
  // The reducers stay pure: they request sounds via Effects.sound and the
  // dispatcher executes them (gated by the per-sound enable flags). These
  // tests drive applyAgentEvent directly to assert the requested effect.

  it("workflow_state_changed → complete requests the ding; other states stay silent", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    const done = applyAgentEvent(useAgentStore.getState(), ID, {
      kind: "workflow_state_changed",
      state: "complete",
      top_plan_id: "p1",
    });
    expect(done.effects?.sound).toBe("complete");
    const mid = applyAgentEvent(useAgentStore.getState(), ID, {
      kind: "workflow_state_changed",
      state: "executing",
      top_plan_id: "p1",
    });
    expect(mid.effects?.sound).toBeUndefined();
  });

  it("approval_request and user_question request the input ping", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    const approval = applyAgentEvent(useAgentStore.getState(), ID, {
      kind: "approval_request",
      tool_call_id: "c1",
      tool_name: "shell",
      args: {},
      preview: null,
      core_operation: false,
    });
    expect(approval.effects?.sound).toBe("input");
    const question = applyAgentEvent(useAgentStore.getState(), ID, {
      kind: "user_question",
      question_id: "q-1",
      question: "Which?",
      options: [
        { label: "A", description: null },
        { label: "B", description: null },
      ],
    });
    expect(question.effects?.sound).toBe("input");
  });

  it("doom: three consecutive errors ending in a final error request the doom sound", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    for (let i = 0; i < DOOM_ERROR_STREAK; i++) {
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: {
          kind: "tool_result",
          tool_call_id: `c${i}`,
          result: { success: false, output: "err" },
        },
      });
    }
    // The streak is at the cap BEFORE the final error lands (the backend
    // emits the abort error after the third failed tool result).
    expect(agent(ID).consecutiveToolErrors).toBe(DOOM_ERROR_STREAK);
    const r = applyAgentEvent(useAgentStore.getState(), ID, {
      kind: "error",
      error: "Aborting turn: 3 consecutive tool errors (the model may be stuck).",
      retrying: false,
    });
    expect(r.effects?.sound).toBe("doom");
    // applyAgentEvent returns the whole next AppState (`next`), not the
    // per-agent ReducerResult — read the agent out of the map.
    expect(r.next.agents[ID]?.running).toBe(false);
  });

  it("a single fatal error stays silent (no doom)", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    const r = applyAgentEvent(useAgentStore.getState(), ID, {
      kind: "error",
      error: "provider unreachable",
      retrying: false,
    });
    expect(r.effects?.sound).toBeUndefined();
    // Even with one earlier failed tool call, the streak (2) is below the cap.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "c0", result: { success: false, output: "x" } },
    });
    const r2 = applyAgentEvent(useAgentStore.getState(), ID, {
      kind: "error",
      error: "boom",
      retrying: false,
    });
    expect(r2.effects?.sound).toBeUndefined();
  });

  it("a successful tool call resets the streak — no doom", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "c0", result: { success: false, output: "x" } },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "c1", result: { success: false, output: "y" } },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "c2", result: { success: true, output: "ok" } },
    });
    expect(agent(ID).consecutiveToolErrors).toBe(0);
    const r = applyAgentEvent(useAgentStore.getState(), ID, {
      kind: "error",
      error: "boom",
      retrying: false,
    });
    expect(r.effects?.sound).toBeUndefined();
  });

  it("parked stores the pre-stall evidence; the next started clears it (backlog 5c33e945)", () => {
    // The backend `parked` event: the agent went idle awaiting input — an
    // interrupt (user stop) or an exhausted auto-continue budget must
    // surface as the InputBar banner, not look like a hang (2027-01-07
    // live: a mid-Executing stop looked like a hang and needed a manual
    // "c" to resume).
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "parked",
        reason: "interrupted",
        workflow_state: "Executing",
        descendants_running: false,
        auto_continue_streak: 3,
      },
    });
    expect(agent(ID).parked).toEqual({
      reason: "interrupted",
      workflow_state: "Executing",
      descendants_running: false,
      auto_continue_streak: 3,
    });
    // The next turn clears the banner — the agent is working again.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    expect(agent(ID).parked).toBeNull();
  });

  it("parked by-design waits are stored as evidence (no banner reason)", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "parked",
        reason: "waiting_for_descendants",
        workflow_state: "Reviewing",
        descendants_running: true,
        auto_continue_streak: 0,
      },
    });
    expect(agent(ID).parked?.reason).toBe("waiting_for_descendants");
    expect(agent(ID).parked?.descendants_running).toBe(true);
  });

  it("doom on provider exhaustion: retry re-Starts never reset the streak (review finding 1)", () => {
    // The REAL backend sequence on provider exhaustion: each retry re-runs
    // the WHOLE turn, re-emitting Started between attempts (a retrying error
    // keeps running=true and no Finished is emitted until the terminal
    // event). The earlier version of this test fed 3 errors with no
    // interleaved Starteds — an impossible stream that passed while the real
    // path was broken (Started reset the streak each time).
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "error", error: "r1", retrying: true } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } }); // attempt 2
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "error", error: "r2", retrying: true } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } }); // attempt 3
    const r = applyAgentEvent(useAgentStore.getState(), ID, {
      kind: "error",
      error: "exhausted",
      retrying: false,
    });
    expect(r.effects?.sound).toBe("doom");
  });

  it("started resets the streak — a fresh turn never inherits old errors", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "c0", result: { success: false, output: "x" } },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "c1", result: { success: false, output: "y" } },
    });
    // A FRESH turn always follows a Finished (running flips false), so this
    // Started is an idle→running one and must reset the streak. (A mid-turn
    // Started — provider retry — must NOT; see the exhaustion test above.)
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "finished", reason: "stop" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    expect(agent(ID).consecutiveToolErrors).toBe(0);
    const r = applyAgentEvent(useAgentStore.getState(), ID, {
      kind: "error",
      error: "boom",
      retrying: false,
    });
    expect(r.effects?.sound).toBeUndefined();
  });

  it("user denials do not extend the doom streak (backend parity, review finding 2)", () => {
    // DenyAll forwards each denial as a failed tool_result — the backend
    // deliberately excludes them from MAX_RETRIES (a deliberate safety
    // choice, not a stuck model), so the frontend streak must too.
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    for (let i = 0; i < 5; i++) {
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: {
          kind: "tool_result",
          tool_call_id: `c${i}`,
          result: { success: false, output: "error: user denied this tool call" },
        },
      });
    }
    expect(agent(ID).consecutiveToolErrors).toBe(0);
    // A real failure still counts.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_result", tool_call_id: "c9", result: { success: false, output: "boom" } },
    });
    expect(agent(ID).consecutiveToolErrors).toBe(1);
  });

  it("isUserDenialToolOutput mirrors the backend substrings exactly", () => {
    expect(isUserDenialToolOutput("error: user denied this tool call")).toBe(true);
    expect(isUserDenialToolOutput("interrupted while awaiting approval")).toBe(true);
    expect(isUserDenialToolOutput("cancelled while awaiting approval")).toBe(true);
    expect(isUserDenialToolOutput("approval channel closed")).toBe(true);
    expect(isUserDenialToolOutput("interrupted: not run")).toBe(true);
    expect(isUserDenialToolOutput("Error: invalid arguments")).toBe(false);
    expect(isUserDenialToolOutput("")).toBe(false);
  });

  it("child_finished: marks not running, appends done note, records child name", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "child_finished", child_id: SUB, name: "reviewer", success: true },
    });
    const a = agent(ID);
    expect(a.running).toBe(false);
    expect(a.transcript.some((e) => e.kind === "assistant" && e.text === "✓ Background task finished.")).toBe(true);
    expect(useAgentStore.getState().agentNames[SUB]).toBe("reviewer");
  });

  it("clearConversation: wipes transcript/streaming/steers/pendingApproval", () => {
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "started" } });
    useAgentStore.getState().handleAgentEvent({ agent_id: ID, event: { kind: "text_delta", text: "partial response" } });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "approval_request", tool_call_id: "call-1", tool_name: "shell", args: {}, preview: null, core_operation: false },
    });
    useAgentStore.getState().addSteer(ID, "do this instead");

    const before = agent(ID);
    // tool_call_start flushes streaming text into the transcript, so the
    // "partial response" is an assistant entry, not pending streaming text.
    expect(before.streamingText).toBe("");
    expect(before.transcript.some((e) => e.kind === "assistant" && e.text === "partial response")).toBe(true);
    expect(before.steers).toHaveLength(1);
    expect(before.pendingApproval).not.toBeNull();
    expect(before.transcript.some((e) => e.kind === "tool")).toBe(true);

    useAgentStore.getState().clearConversation(ID);
    const after = agent(ID);
    expect(after.transcript).toHaveLength(0);
    expect(after.streamingText).toBe("");
    expect(after.steers).toHaveLength(0);
    expect(after.pendingApproval).toBeNull();
  });

  it("removeSteer: removes a single pending steer by id (the x on a backlog entry)", () => {
    useAgentStore.getState().addSteer(ID, "first");
    useAgentStore.getState().addSteer(ID, "second");
    const before = agent(ID);
    expect(before.steers).toHaveLength(2);
    const firstId = before.steers[0].id;

    useAgentStore.getState().removeSteer(ID, firstId);

    const after = agent(ID);
    expect(after.steers).toHaveLength(1);
    expect(after.steers[0].text).toBe("second");
  });

  it("backlogDraft setters: text + images persist in the store (survive BacklogInput unmount)", () => {
    // Regression: BacklogInput's draft used to be component useState, wiped
    // when switching away from the Backlog tab unmounted the component. Now
    // the draft lives in the store, so it must round-trip through setters.
    const s = useAgentStore.getState();
    expect(s.backlogDraft).toBe("");
    expect(s.backlogDraftImages).toEqual([]);
    s.setBacklogDraft("half-typed prompt");
    s.setBacklogDraftImages(["data:image/png;base64,AAA", "data:image/png;base64,BBB"]);
    let cur = useAgentStore.getState();
    expect(cur.backlogDraft).toBe("half-typed prompt");
    expect(cur.backlogDraftImages).toEqual(["data:image/png;base64,AAA", "data:image/png;base64,BBB"]);
    // Clearing (as handleAdd does after a successful add) empties both.
    cur.setBacklogDraft("");
    cur.setBacklogDraftImages([]);
    cur = useAgentStore.getState();
    expect(cur.backlogDraft).toBe("");
    expect(cur.backlogDraftImages).toEqual([]);
  });
});

describe("aggregateTokPerSec", () => {
  it("returns null when there's no timing data (msTotal = 0)", () => {
    expect(aggregateTokPerSec(100, 0)).toBeNull();
  });

  it("computes tokens / total time (100 tokens / 1000ms = 100 tok/s)", () => {
    expect(aggregateTokPerSec(100, 1000)).toBeCloseTo(100);
  });

  it("regression: N requests of equal throughput do NOT inflate by N", () => {
    // 10 requests, each 100 tokens / 1000ms. Total = 1000 tokens / 10000ms.
    // Correct aggregate throughput = 100 tok/s. The old buggy formula
    // (tokens / avg-per-request-time) gave 1000 tok/s — 10× too high.
    const tokens = 10 * 100;
    const msTotal = 10 * 1000;
    expect(aggregateTokPerSec(tokens, msTotal)).toBeCloseTo(100);
  });
});

/**
 * Regression (F3, 2026-04-19 freeze diagnosis): `transcript` and
 * `activityLog` grew without bound, so per-delta agent spreads + transcript
 * re-renders got linearly slower as a session lengthened. Both are now
 * keep-last-N capped at the mutation sites (a `.slice(-N)` keep-last-N cap).
 */
describe("transcript + activityLog caps (F3 regression)", () => {
  beforeEach(resetStore);

  it("transcript: pushing past MAX_TRANSCRIPT_ENTRIES keeps the last N, oldest-first", () => {
    const total = MAX_TRANSCRIPT_ENTRIES + 3;
    for (let i = 0; i < total; i++) {
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: { kind: "prompt_dispatched", text: `prompt-${i}`, images: [] },
      });
    }
    const t = agent(ID).transcript;
    expect(t).toHaveLength(MAX_TRANSCRIPT_ENTRIES);
    // Oldest 3 dropped; order preserved (oldest kept first, newest last).
    expect(t[0]).toMatchObject({ kind: "user", text: "prompt-3" });
    expect(t[t.length - 1]).toMatchObject({ kind: "user", text: `prompt-${total - 1}` });
  });

  it("activityLog: pushing past MAX_ACTIVITY_ENTRIES keeps the last N, oldest-first", () => {
    const total = MAX_ACTIVITY_ENTRIES + 3;
    for (let i = 0; i < total; i++) {
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: { kind: "error", error: `err-${i}`, retrying: true },
      });
    }
    const log = agent(ID).activityLog;
    expect(log).toHaveLength(MAX_ACTIVITY_ENTRIES);
    // Oldest 3 dropped; order preserved.
    expect(log[0]).toMatchObject({ kind: "error", text: "err-3" });
    expect(log[log.length - 1]).toMatchObject({ kind: "error", text: `err-${total - 1}` });
    // Each error also appended one transcript entry (303 total — under the
    // 1000 cap, so the transcript cap itself isn't hit here; the first test
    // in this block exercises it).
    expect(agent(ID).transcript).toHaveLength(total);
  });
});

/**
 * Stable transcript-entry ids (mem-perf review HIGH 3): Conversation keys
 * its turn/run/entry lists by entryId — index keys misattribute row state
 * once the transcript hits its 1000-entry cap (every append shifts all
 * indices, so a Message row's key lands on a DIFFERENT entry:
 * arePropsEqual fails → deep re-render; local row state like an expanded
 * tool card silently transfers). The ids are stamped at creation by the
 * applyAgentEvent identity pass (plus the direct-push sites) and must
 * survive capTranscript's slice and the spread-based entry replacements
 * (arg deltas, finalizations).
 */
describe("transcript entryIds (mem-perf review HIGH 3)", () => {
  beforeEach(resetStore);

  it("applyAgentEvent stamps new entries with monotonic entryIds", () => {
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "prompt_dispatched", text: "p", images: [] },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "error", error: "e", retrying: true },
    });
    const t = agent(ID).transcript;
    expect(t).toHaveLength(2);
    expect(t[0].entryId).toBeDefined();
    expect(t[1].entryId).toBeDefined();
    expect(t[0].entryId!).toBeLessThan(t[1].entryId!);
  });

  it("entry replacements keep their entryId (arg deltas spread the entry)", () => {
    // The tool entry object is replaced per arg delta (spread) — the id
    // must survive or the row's React key changes mid-stream.
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_start", index: 0, id: "call-1", name: "shell" },
    });
    const before = agent(ID).transcript[0];
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: { kind: "tool_call_arg_delta", index: 0, fragment: "ls" },
    });
    const after = agent(ID).transcript[0];
    expect(after).not.toBe(before); // replaced object…
    expect(after.entryId).toBe(before.entryId); // …stable id
  });

  it("ids stay stable + monotonic across the transcript cap", () => {
    const total = MAX_TRANSCRIPT_ENTRIES + 3;
    let idBeforeCap: number | undefined;
    for (let i = 0; i < total; i++) {
      useAgentStore.getState().handleAgentEvent({
        agent_id: ID,
        event: { kind: "prompt_dispatched", text: `prompt-${i}`, images: [] },
      });
      // The entry appended at index MAX-1 is the last one before the cap
      // starts dropping — capture its id.
      if (i === MAX_TRANSCRIPT_ENTRIES - 1) {
        idBeforeCap = agent(ID).transcript[MAX_TRANSCRIPT_ENTRIES - 1].entryId;
      }
    }
    const t = agent(ID).transcript;
    expect(t).toHaveLength(MAX_TRANSCRIPT_ENTRIES);
    // Monotonic across the survivors…
    for (let i = 1; i < t.length; i++) {
      expect(t[i - 1].entryId!).toBeLessThan(t[i].entryId!);
    }
    // …and stable: the oldest 3 dropped, and the entry that was last before
    // the cap started biting keeps its id at its shifted position (its
    // index moved, its id didn't).
    expect(t[0]).toMatchObject({ kind: "user", text: "prompt-3" });
    expect(idBeforeCap).toBeDefined();
    expect(t[t.length - 4].entryId).toBe(idBeforeCap);
  });

  it("recordQuestionAnswer (the direct reduceQuestionAnswered path) stamps an entryId", () => {
    // reduceQuestionAnswered is invoked directly by the store action,
    // bypassing applyAgentEvent's identity pass — it stamps both fields
    // itself (ts + entryId).
    useAgentStore.getState().handleAgentEvent({
      agent_id: ID,
      event: {
        kind: "user_question",
        question_id: "q-1",
        question: "Favorite color?",
        options: [{ label: "Blue", description: null }],
      },
    });
    useAgentStore.getState().recordQuestionAnswer(ID, {
      questionId: "q-1",
      question: "Favorite color?",
      answer: "Blue",
    });
    const qa = agent(ID).transcript.find((e) => e.kind === "qa");
    expect(qa).toBeDefined();
    expect(qa?.entryId).toBeDefined();
  });

  it("stampEntryIds stamps id-less entries in order and passes stamped ones through", () => {
    const stamped: TranscriptEntry[] = [
      { kind: "user", text: "a", entryId: 41 },
      { kind: "assistant", text: "b" },
      { kind: "assistant", text: "c" },
    ];
    const out = stampEntryIds(stamped);
    // Already-stamped entries keep their object identity…
    expect(out[0]).toBe(stamped[0]);
    // …id-less ones get ids in increasing (transcript) order — above the
    // foreign 41, which syncs the counter, so this holds in any execution
    // order (review LOW 2, plan 225e0dad).
    expect(out[1].entryId!).toBeGreaterThan(stamped[0].entryId!);
    expect(out[1].entryId!).toBeLessThan(out[2].entryId!);
    // A fully-stamped input returns the SAME array (no re-creation).
    expect(stampEntryIds(out)).toBe(out);
  });

  it("stampEntryIds advances the counter past foreign ids (no duplicate keys after /load)", () => {
    // A /load restore imports ids from a saved conversation; without
    // syncing the counter, the next allocation could collide with a
    // loaded id → duplicate React keys, the misattribution class this
    // plan eliminates (review HIGH 1, plan 225e0dad).
    const foreign: TranscriptEntry[] = [
      { kind: "user", text: "a", entryId: 5000 },
      { kind: "assistant", text: "b" },
    ];
    const out = stampEntryIds(foreign);
    expect(out[0].entryId).toBe(5000); // foreign id preserved
    expect(out[1].entryId).toBeGreaterThan(5000); // stamped above it
    expect(allocEntryId()).toBeGreaterThan(5000); // counter advanced
  });
});

/**
 * Regression (review L3, 2026-04-19 freeze-fix review): the F1b turn-end
 * edge — the git-branch refresh in App.tsx must fire ONLY on the main
 * agent's running true→false edge. The logic is extracted here as the pure
 * `didMainTurnEnd(prev, next)` so every transition is testable.
 */
describe("didMainTurnEnd (F1b turn-end edge, review L3)", () => {
  /** Build a minimal snapshot: one main agent (parent null) + optional subs. */
  function snap(mainRunning: boolean | null, mainId: AgentId = ID): MainRunningSnapshot {
    return {
      agentParents: { [mainId]: null },
      agents:
        mainRunning === null
          ? {}
          : { [mainId]: { ...emptyAgentState(), running: mainRunning } },
    };
  }

  it("false→false, false→true, true→true do not fire", () => {
    expect(didMainTurnEnd(snap(false), snap(false))).toBe(false);
    expect(didMainTurnEnd(snap(false), snap(true))).toBe(false);
    expect(didMainTurnEnd(snap(true), snap(true))).toBe(false);
  });

  it("true→false fires (the turn resolved)", () => {
    expect(didMainTurnEnd(snap(true), snap(false))).toBe(true);
  });

  it("main agent removed between snapshots while running fires (reads as false)", () => {
    expect(didMainTurnEnd(snap(true), snap(null))).toBe(true);
  });

  it("main id shifting to a different non-running agent fires", () => {
    const prev = snap(true, 1 as AgentId);
    // Next snapshot: agent 1 gone; agent 2 is now the parentless (main) one
    // and is not running.
    const next: MainRunningSnapshot = {
      agentParents: { 2: null },
      agents: { 2: { ...emptyAgentState(), running: false } },
    };
    expect(didMainTurnEnd(prev, next)).toBe(true);
  });

  it("no agents at all never fires", () => {
    expect(didMainTurnEnd(snap(null), snap(null))).toBe(false);
  });
});

/**
 * The right-panel tab actions the File viewer's deep-links rely on
 * (review LOW #6). `revealRightPanelTab` enables + selects + reveals a tab
 * WITHOUT toggling it off; `requestFileOpen` reveals the Files tab and holds
 * the path in `pendingFileOpen` for the FileViewer to consume (race-free).
 */
describe("revealRightPanelTab + requestFileOpen", () => {
  /** Set the tab slices to a known state for each test. */
  function setTabs(partial: {
    rightPanelTab?: "plan" | "files" | "diff";
    disabledTabs?: ("plan" | "files" | "diff")[];
    rightPanelVisible?: boolean;
    pendingFileOpen?: { path: string; line: number | null } | null;
  }) {
    useAgentStore.setState({
      rightPanelTab: "plan",
      disabledTabs: [],
      rightPanelVisible: true,
      pendingFileOpen: null,
      ...partial,
    });
  }

  it("revealRightPanelTab enables a disabled tab, selects it, and reveals the panel", () => {
    setTabs({ rightPanelTab: "plan", disabledTabs: ["files"], rightPanelVisible: false });
    useAgentStore.getState().revealRightPanelTab("files");
    const s = useAgentStore.getState();
    expect(s.rightPanelTab).toBe("files");
    expect(s.disabledTabs).not.toContain("files");
    expect(s.rightPanelVisible).toBe(true);
  });

  it("revealRightPanelTab NEVER disables an already-enabled tab (regression vs toggleTabAndReveal)", () => {
    setTabs({ rightPanelTab: "files", disabledTabs: [], rightPanelVisible: true });
    useAgentStore.getState().revealRightPanelTab("files");
    const s = useAgentStore.getState();
    expect(s.rightPanelTab).toBe("files");
    expect(s.disabledTabs).not.toContain("files");
    expect(s.rightPanelVisible).toBe(true);
  });

  it("requestFileOpen reveals + selects the Files tab and holds the pending path", () => {
    setTabs({ rightPanelTab: "plan", disabledTabs: ["files"], rightPanelVisible: false });
    useAgentStore.getState().requestFileOpen("src/main.rs");
    const s = useAgentStore.getState();
    expect(s.rightPanelTab).toBe("files");
    expect(s.disabledTabs).not.toContain("files");
    expect(s.rightPanelVisible).toBe(true);
    expect(s.pendingFileOpen).toEqual({ path: "src/main.rs", line: null });
  });

  it("requestFileOpen carries the deep-link line (read_files links open at the read line)", () => {
    setTabs({ rightPanelTab: "plan", disabledTabs: ["files"], rightPanelVisible: false });
    useAgentStore.getState().requestFileOpen("src/main.rs", 42);
    expect(useAgentStore.getState().pendingFileOpen).toEqual({
      path: "src/main.rs",
      line: 42,
    });
  });

  it("clearPendingFileOpen clears the pending path", () => {
    setTabs({ pendingFileOpen: { path: "src/main.rs", line: 42 } });
    useAgentStore.getState().clearPendingFileOpen();
    expect(useAgentStore.getState().pendingFileOpen).toBeNull();
  });
});
