// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect, vi } from "vitest";
import {
  createAgentEventDispatcher,
  drainStreamingBuffer,
  deliverAgentEvent,
  attachAgentDispatch,
  detachAgentDispatch,
} from "./useAgentEvents";
import { MAX_FLUSH_INTERVAL_MS } from "./deltaFlush";
import type { AgentId, AgentEventPayload } from "../lib/types";

/**
 * WIRING regression guard for R2 (review F6, 2026-08-18) + the hybrid flush
 * policy (2026-08-22: "the display must stream as the model streams" — both
 * the reasoning display and the agent window update as soon as a token
 * arrives, even when requestAnimationFrame stalls under RDP occlusion /
 * WebView2 quirks). The pure `shouldFlushSync` / `shouldFlushNow` predicate
 * tests alone could not catch deleting the routing calls from the
 * dispatcher — these tests drive the REAL dispatcher (extracted from the
 * hook with injected deps: frame loop, timers, clock) and assert the policy
 * itself: the first delta flushes immediately, dense deltas batch per frame,
 * a stalled frame loop is compensated by the time-gap immediate flush and by
 * the fallback timer, and the 64 KiB occlusion cap still forces a
 * synchronous flush.
 */

// The dispatcher only calls `listAgents` (unknown-agent name resolution);
// useAgentEvents imports only a *type* from this module (plus the three
// listener-registration functions, which only run inside the mounted hook —
// never in these dispatcher-level tests), so mocking these exports is
// sufficient and keeps @tauri-apps/api out of the node env.
vi.mock("../lib/tauri", () => ({
  onAgentEvent: vi.fn(),
  onBacklogChanged: vi.fn(),
  onBrowserReveal: vi.fn(),
  backlogList: vi.fn(async () => []),
  listAgents: vi.fn(async () => []),
  uiDiag: vi.fn(async () => {}),
}));

/** Minimal raf/caf pair that captures scheduled frames for manual firing. */
function makeRaf() {
  const frames: Array<{ id: number; cb: () => void }> = [];
  let nextId = 1;
  const raf = (cb: () => void): number => {
    const id = nextId++;
    frames.push({ id, cb });
    return id;
  };
  const caf = (id: number): void => {
    const i = frames.findIndex((f) => f.id === id);
    if (i >= 0) frames.splice(i, 1);
  };
  /** Run every currently scheduled frame exactly once. */
  const fire = (): void => {
    for (const f of frames.splice(0)) f.cb();
  };
  return { raf, caf, fire, count: () => frames.length };
}

/** Minimal setTimeout/clearTimeout pair capturing armed timers for manual
 *  firing (injected instead of the browser globals so tests stay in node). */
function makeTimers() {
  const timers: Array<{ id: number; cb: () => void; ms: number }> = [];
  let nextId = 1;
  const setTimer = (cb: () => void, ms: number): number => {
    const id = nextId++;
    timers.push({ id, cb, ms });
    return id;
  };
  const clearTimer = (id: number): void => {
    const i = timers.findIndex((t) => t.id === id);
    if (i >= 0) timers.splice(i, 1);
  };
  /** Run every currently armed timer exactly once (regardless of delay). */
  const fire = (): void => {
    for (const t of timers.splice(0)) t.cb();
  };
  return { setTimer, clearTimer, fire, count: () => timers.length };
}

interface Recorder {
  text: Array<[AgentId, string]>;
  reasoning: Array<[AgentId, string]>;
  /** Flushed live-output batches, in flush order. */
  output: Array<[AgentId, Array<{ tool_call_id: string; text: string }>]>;
  /** Non-delta events handed to the store, in dispatch order. */
  structural: AgentEventPayload[];
  /** Interleaved log of output flushes and structural dispatches. */
  order: string[];
  dispatcher: ReturnType<typeof createAgentEventDispatcher>;
  rafTools: ReturnType<typeof makeRaf>;
  timerTools: ReturnType<typeof makeTimers>;
  /** Advance the injected clock by `ms`. */
  advance: (ms: number) => void;
}

/** Build a dispatcher with recording store actions + a stubbed frame loop,
 *  timer set, and clock (starting at t=1000 so the dispatcher's zeroed
 *  `lastFlushAt` makes the first delta a time-gap immediate flush, exactly
 *  as in production where Date.now() is huge). */
function makeDispatcher(): Recorder {
  const rafTools = makeRaf();
  const timerTools = makeTimers();
  let clock = 1000;
  const now = () => clock;
  const text: Array<[AgentId, string]> = [];
  const reasoning: Array<[AgentId, string]> = [];
  const output: Array<[AgentId, Array<{ tool_call_id: string; text: string }>]> = [];
  const structural: AgentEventPayload[] = [];
  const order: string[] = [];
  const dispatcher = createAgentEventDispatcher({
    textBuffers: new Map(),
    reasoningBuffers: new Map(),
    argBuffers: new Map(),
    outputBuffers: new Map(),
    rafId: { current: 0 },
    timerId: { current: null },
    raf: rafTools.raf,
    caf: rafTools.caf,
    setTimer: timerTools.setTimer,
    clearTimer: timerTools.clearTimer,
    now,
    appendStreamingText: (id, t) => {
      text.push([id, t]);
    },
    appendStreamingReasoning: (id, t) => {
      reasoning.push([id, t]);
    },
    applyToolCallArgDeltas: () => {},
    applyToolOutputDeltas: (id, deltas) => {
      output.push([id, deltas]);
      for (const d of deltas) order.push(`output:${id}:${d.tool_call_id}:${d.text}`);
    },
    handleAgentEvent: (payload) => {
      structural.push(payload);
      order.push(`event:${payload.event.kind}`);
    },
  });
  return {
    text,
    reasoning,
    output,
    structural,
    order,
    dispatcher,
    rafTools,
    timerTools,
    advance: (ms) => (clock += ms),
  };
}

function textDelta(id: AgentId, text: string): AgentEventPayload {
  return { agent_id: id, event: { kind: "text_delta", text } };
}

function reasoningDelta(id: AgentId, text: string): AgentEventPayload {
  return { agent_id: id, event: { kind: "reasoning_delta", text } };
}

/** A live-output chunk for one running call. */
function outputDelta(id: AgentId, toolCallId: string, text: string): AgentEventPayload {
  return {
    agent_id: id,
    event: { kind: "tool_output_delta", tool_call_id: toolCallId, stream: "stdout", text },
  };
}

/** The result that ends `toolCallId` — the structural event whose ordering
 *  against a buffered live chunk is pinned below. */
function toolResult(id: AgentId, toolCallId: string): AgentEventPayload {
  return {
    agent_id: id,
    event: {
      kind: "tool_result",
      tool_call_id: toolCallId,
      result: { success: true, output: "done" },
    },
  };
}

describe("createAgentEventDispatcher delta buffering (hybrid flush policy)", () => {
  it("the FIRST delta flushes immediately — no frame or timer required", () => {
    const { text, dispatcher, rafTools, timerTools } = makeDispatcher();
    dispatcher.dispatch(textDelta(1, "hel"));
    expect(text).toEqual([[1, "hel"]]);
    expect(rafTools.count()).toBe(0);
    expect(timerTools.count()).toBe(0);
  });

  it("a delta arriving < MAX_FLUSH_INTERVAL_MS later batches and waits for the frame (fallback timer armed)", () => {
    const { text, dispatcher, rafTools, timerTools, advance } = makeDispatcher();
    dispatcher.dispatch(textDelta(1, "hel"));
    advance(MAX_FLUSH_INTERVAL_MS - 30);
    dispatcher.dispatch(textDelta(1, "lo"));
    // Dense burst: still buffered — one pending frame, one armed timer.
    expect(text).toEqual([[1, "hel"]]);
    expect(rafTools.count()).toBe(1);
    expect(timerTools.count()).toBe(1);
    // The frame fires first → flush → the fallback timer is cancelled.
    rafTools.fire();
    expect(text).toEqual([
      [1, "hel"],
      [1, "lo"],
    ]);
    expect(timerTools.count()).toBe(0);
  });

  it("stalled rAF: a delta arriving >= MAX_FLUSH_INTERVAL_MS after the last flush lands IMMEDIATELY", () => {
    const { text, dispatcher, rafTools, timerTools, advance } = makeDispatcher();
    dispatcher.dispatch(textDelta(1, "hel"));
    advance(MAX_FLUSH_INTERVAL_MS);
    // No frame ever fires (RDP occlusion) — the time-gap route still flushes.
    dispatcher.dispatch(textDelta(1, "lo"));
    expect(text).toEqual([
      [1, "hel"],
      [1, "lo"],
    ]);
    expect(rafTools.count()).toBe(0);
    expect(timerTools.count()).toBe(0);
  });

  it("stalled rAF + silence: the fallback TIMER flushes the buffered batch", () => {
    const { text, dispatcher, rafTools, timerTools, advance } = makeDispatcher();
    dispatcher.dispatch(textDelta(1, "hel"));
    advance(10);
    dispatcher.dispatch(textDelta(1, "lo"));
    expect(text).toEqual([[1, "hel"]]);
    expect(rafTools.count()).toBe(1);
    expect(timerTools.count()).toBe(1);
    // The frame never fires; the fallback timer does — and cancels the frame.
    timerTools.fire();
    expect(text).toEqual([
      [1, "hel"],
      [1, "lo"],
    ]);
    expect(rafTools.count()).toBe(0);
    expect(timerTools.count()).toBe(0);
    // A later frame firing must not re-append (first-wins, no double flush).
    rafTools.fire();
    expect(text).toEqual([
      [1, "hel"],
      [1, "lo"],
    ]);
  });

  it("a >64 KiB buffer flushes SYNCHRONOUSLY even inside the batch window — no frame required", () => {
    const { text, dispatcher, rafTools, timerTools, advance } = makeDispatcher();
    const small = "s";
    const big = "y".repeat(64 * 1024); // small + big crosses the cap
    dispatcher.dispatch(textDelta(1, small));
    advance(10);
    dispatcher.dispatch(textDelta(1, big));
    // The over-cap path flushed the whole buffer in one append with no frame.
    expect(text).toEqual([
      [1, small],
      [1, big],
    ]);
    expect(rafTools.count()).toBe(0);
    expect(timerTools.count()).toBe(0);
  });

  it("a sync flush cancels the pending frame AND timer — no double flush", () => {
    const { text, dispatcher, rafTools, timerTools, advance } = makeDispatcher();
    dispatcher.dispatch(textDelta(1, "hel"));
    advance(10);
    dispatcher.dispatch(textDelta(1, "lo"));
    expect(rafTools.count()).toBe(1);
    expect(timerTools.count()).toBe(1);
    advance(MAX_FLUSH_INTERVAL_MS);
    // The time-gap immediate flush cancels the pending frame + timer.
    dispatcher.dispatch(textDelta(1, "!"));
    expect(text).toEqual([
      [1, "hel"],
      [1, "lo!"],
    ]);
    expect(rafTools.count()).toBe(0);
    expect(timerTools.count()).toBe(0);
    // Neither a late frame nor a late timer may re-append.
    rafTools.fire();
    timerTools.fire();
    expect(text).toEqual([
      [1, "hel"],
      [1, "lo!"],
    ]);
  });

  it("reasoning deltas follow the same policy — first one lands immediately", () => {
    const { reasoning, dispatcher, rafTools, timerTools } = makeDispatcher();
    dispatcher.dispatch(reasoningDelta(1, "thinking…"));
    expect(reasoning).toEqual([[1, "thinking…"]]);
    expect(rafTools.count()).toBe(0);
    expect(timerTools.count()).toBe(0);
  });

  it("live tool output coalesces per flush, one store write per running call", () => {
    const { output, dispatcher, rafTools } = makeDispatcher();
    // The first chunk flushes immediately (same hybrid policy as the other
    // deltas) — the card shows the first line without waiting for a frame.
    dispatcher.dispatch(outputDelta(1, "call-1", "one\n"));
    expect(output).toEqual([[1, [{ tool_call_id: "call-1", text: "one\n" }]]]);
    // Dense follow-ups (a chatty command's 8 KiB chunks) buffer instead of
    // re-rendering the transcript per chunk, and coalesce per call.
    dispatcher.dispatch(outputDelta(1, "call-1", "two\n"));
    dispatcher.dispatch(outputDelta(1, "call-1", "three\n"));
    dispatcher.dispatch(outputDelta(1, "call-2", "other\n"));
    expect(output.length).toBe(1);
    rafTools.fire();
    expect(output[1]).toEqual([
      1,
      [
        { tool_call_id: "call-1", text: "two\nthree\n" },
        { tool_call_id: "call-2", text: "other\n" },
      ],
    ]);
  });

  it("a buffered live chunk flushes BEFORE the structural event that follows it — never after the result", () => {
    const { order, dispatcher } = makeDispatcher();
    dispatcher.dispatch(outputDelta(1, "call-1", "one\n")); // immediate flush
    dispatcher.dispatch(outputDelta(1, "call-1", "two\n")); // buffered
    dispatcher.dispatch(toolResult(1, "call-1"));
    // The pending chunk reaches the store as its own write BEFORE the result
    // is dispatched, so a live tail can never land in a card whose result has
    // already replaced it (the reducer's own guard is the second line of
    // defence, for chunks that lose the race on the Rust side).
    expect(order).toEqual([
      "output:1:call-1:one\n",
      "output:1:call-1:two\n",
      "event:tool_result",
    ]);
  });

  it("clearStreamingBuffer drains buffers AND cancels the pending frame + fallback timer", () => {
    const textBuffers = new Map<AgentId, string>([[1, "pending text"]]);
    const reasoningBuffers = new Map<AgentId, string>([[1, "pending reasoning"]]);
    const argBuffers = new Map<AgentId, Map<number, string>>([
      [1, new Map([[0, "frag"]])],
    ]);
    const outputBuffers = new Map<AgentId, Map<string, string>>([
      [1, new Map([["call-1", "live output"]])],
    ]);
    const rafIdRef = { current: 7 };
    const timerIdRef: { current: ReturnType<typeof setTimeout> | null } = { current: 42 };
    const cancelledFrames: number[] = [];
    const cancelledTimers: Array<ReturnType<typeof setTimeout>> = [];
    drainStreamingBuffer(
      { textBuffers, reasoningBuffers, argBuffers, outputBuffers, rafIdRef, timerIdRef },
      1,
      {
        cancelFrame: (id) => cancelledFrames.push(id),
        cancelTimer: (id) => cancelledTimers.push(id),
      },
    );
    // Every buffer for the cleared agent is drained…
    expect(textBuffers.size).toBe(0);
    expect(reasoningBuffers.size).toBe(0);
    expect(argBuffers.size).toBe(0);
    expect(outputBuffers.size).toBe(0);
    // …and the pending frame + fallback timer are cancelled and zeroed, so a
    // later frame/timer fire cannot repopulate the just-cleared conversation.
    expect(cancelledFrames).toEqual([7]);
    expect(cancelledTimers).toEqual([42]);
    expect(rafIdRef.current).toBe(0);
    expect(timerIdRef.current).toBe(null);
  });

  it("clearStreamingBuffer leaves the pending flush alone when another agent still has buffered deltas", () => {
    const textBuffers = new Map<AgentId, string>([[2, "other agent's text"]]);
    const reasoningBuffers = new Map<AgentId, string>();
    const argBuffers = new Map<AgentId, Map<number, string>>();
    const outputBuffers = new Map<AgentId, Map<string, string>>();
    const rafIdRef = { current: 9 };
    const timerIdRef: { current: ReturnType<typeof setTimeout> | null } = { current: 43 };
    const cancelledFrames: number[] = [];
    const cancelledTimers: Array<ReturnType<typeof setTimeout>> = [];
    drainStreamingBuffer(
      { textBuffers, reasoningBuffers, argBuffers, outputBuffers, rafIdRef, timerIdRef },
      1, // agent 1 had nothing buffered; agent 2 still does
      {
        cancelFrame: (id) => cancelledFrames.push(id),
        cancelTimer: (id) => cancelledTimers.push(id),
      },
    );
    // The pending flush must still fire for agent 2's buffered text.
    expect(textBuffers.get(2)).toBe("other agent's text");
    expect(cancelledFrames).toEqual([]);
    expect(cancelledTimers).toEqual([]);
    expect(rafIdRef.current).toBe(9);
    expect(timerIdRef.current).toBe(43);
  });
});

describe("agent-event delivery with no dispatcher attached", () => {
  // Regression (live 2026-09-08): the module-level Tauri listener outlives
  // every hook instance, but `activeDispatch` is null from an unmount until
  // the next mount — and forever if the owner never remounts. Events were
  // then received and silently discarded: the chat, reasoning display and
  // token counters froze mid-session while the agents kept working and
  // `invoke`-driven UI (tabs, backlog) stayed live. Delivery must buffer,
  // then replay on the next attach.
  const payload = (id: AgentId, text: string): AgentEventPayload =>
    ({ agent_id: id, event: { kind: "text_delta", text } }) as AgentEventPayload;

  it("buffers while nothing is attached and replays in order on attach", () => {
    const seen: AgentEventPayload[] = [];
    // Mount then unmount, exactly as the hook's effect does.
    const gone = () => {};
    attachAgentDispatch(gone);
    detachAgentDispatch(gone);

    deliverAgentEvent(payload(1, "a"));
    deliverAgentEvent(payload(2, "b"));
    expect(seen).toHaveLength(0);

    attachAgentDispatch((p) => seen.push(p));
    expect(seen.map((p) => (p.event as { text: string }).text)).toEqual([
      "a",
      "b",
    ]);
  });

  it("delivers straight through once a dispatcher is attached", () => {
    const seen: AgentEventPayload[] = [];
    attachAgentDispatch((p) => seen.push(p));
    deliverAgentEvent(payload(3, "c"));
    expect(seen).toHaveLength(1);
  });
});
