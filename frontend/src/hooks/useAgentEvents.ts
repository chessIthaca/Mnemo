// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Hook: subscribe to Tauri agent events, update the Zustand store.
//
// `text_delta` events arrive at high frequency (one per token). Updating the
// Zustand store on every token causes excessive re-renders — the entire
// transcript + streaming text re-renders per token. Instead, we buffer
// `text_delta` fragments per agent and flush them to the store once per
// animation frame (~60fps) via `requestAnimationFrame`. This batches dozens
// of tokens into a single store update. If a per-agent buffer grows past
// 64 KiB it flushes SYNCHRONOUSLY instead of waiting for a frame: rAF never
// fires while the window is occluded (RDP desktop switch, covered window),
// so without the cap the deltas accumulate for the whole hidden period and
// flush in one janky burst on return (R2 of the 2026-08-18 RDP freeze
// diagnosis — see deltaFlush.ts).
//
// HYBRID FLUSH POLICY (2026-08-22, user request "display must stream as the
// model streams"): the frame path alone is not enough — a stalled rAF
// (RDP occlusion / WebView2 quirks, upstream #4879) left sub-64 KiB
// responses invisible until a structural event or turn end ("both lag,
// everything appears at once"). On top of the frame batch + 64 KiB cap, the
// dispatcher now (a) flushes IMMEDIATELY when >= MAX_FLUSH_INTERVAL_MS
// elapsed since the last flush — sparse tokens (reasoning models) land
// per-token, and (b) arms a one-shot fallback `setTimeout` beside each rAF,
// so a stalled frame loop can never hold text indefinitely (first-wins,
// each cancels the other). When rAF is healthy the frame fires first and
// cancels the timer — behavior is unchanged; when rAF stalls the timer (or
// the per-delta time-gap check) still flushes on schedule.
//
// Non-text events (tool calls, approvals, finished, etc.) flush any pending
// buffered text synchronously *before* dispatching, so `flushStreamingText`
// in the store sees all accumulated text before a tool card is inserted.
//
// WHY A MODULE-LEVEL SINGLETON LISTENER:
// <React.StrictMode> (main.tsx) mounts → unmounts → re-mounts every component
// in dev, running each useEffect twice. The Tauri `listen()` is async: its
// unlisten function arrives via a promise. The first effect's cleanup runs
// BEFORE that promise resolves, so it can't unsubscribe — leaking a second
// live listener when the effect re-runs. Two listeners both buffer + flush
// every `text_delta`, so each line of the response is appended twice (the
// text appears to overwrite/duplicate itself line after line).
//
// The fix: the Tauri subscription lives at module scope and is created at
// most once for the life of the window. The per-hook effect only registers
// *which* dispatch handler is active. Re-mounting swaps the handler; it never
// opens a second Tauri listener, so duplication is structurally impossible.
//
// The buffering/dispatch logic itself lives in `createAgentEventDispatcher`
// at module scope (dependency-injected) so it is unit-testable without
// React — useAgentEvents.dispatch.test.ts is the R2 wiring regression guard
// (review F6, 2026-08-18).

import { useEffect, useRef } from "react";
import { onAgentEvent, onBacklogChanged, onBrowserReveal, backlogList, listAgents, uiDiag } from "../lib/tauri";
import { useAgentStore } from "./useAgentStore";
import { handleBrowserReveal } from "./browserReveal";
import { shouldFlushSync, shouldFlushNow, MAX_FLUSH_INTERVAL_MS } from "./deltaFlush";
import type { AgentId, AgentEventPayload } from "../lib/types";

/** The currently-active dispatch handler (set by the mounted hook). */
let activeDispatch: ((payload: AgentEventPayload) => void) | null = null;
/**
 * Agent events received while NO dispatcher was attached.
 *
 * The module-level Tauri listener outlives every hook instance, but
 * `activeDispatch` is null from an unmount until the next mount — and if the
 * hook's owner never remounts, it stays null forever. The listener then keeps
 * receiving events and silently discarded them, so the whole Rust->TS agent
 * stream vanished while `invoke`-driven UI (tabs, backlog) kept working: the
 * chat, reasoning and token counters froze mid-session with the app still
 * doing the work behind the scenes (live 2026-09-08, six agents streaming).
 * Buffering instead of dropping makes that failure recoverable — the next
 * mount replays what it missed — and logs it loudly instead of failing mute.
 *
 * Bounded: a UI that never remounts must not grow this without limit. On
 * overflow the OLDEST payloads go first (recent state is what the transcript
 * needs to catch up).
 */
const pendingPayloads: AgentEventPayload[] = [];
/** Max buffered payloads while no dispatcher is attached (see above). */
const PENDING_PAYLOAD_CAP = 2000;
/** Whether the no-dispatcher condition has already been logged (once per
 *  episode — one line, not one per event). */
let loggedMissingDispatch = false;
/** Whether the first-delivery diagnostic has been reported. */
let reportedFirstDelivery = false;

/**
 * Route one agent event to the active dispatcher, or buffer it when none is
 * attached. Exported for tests (the module listener is not injectable).
 */
export function deliverAgentEvent(payload: AgentEventPayload): void {
  if (!reportedFirstDelivery) {
    reportedFirstDelivery = true;
    void uiDiag("first agent event DELIVERED to the frontend listener");
  }
  if (activeDispatch) {
    activeDispatch(payload);
    return;
  }
  if (!loggedMissingDispatch) {
    loggedMissingDispatch = true;
    console.error(
      "agent events are arriving with no dispatcher attached — buffering " +
        "until useAgentEvents remounts (UI would otherwise freeze silently)"
    );
  }
  pendingPayloads.push(payload);
  if (pendingPayloads.length > PENDING_PAYLOAD_CAP) {
    pendingPayloads.splice(0, pendingPayloads.length - PENDING_PAYLOAD_CAP);
  }
}

/**
 * Detach `dispatch` if it is still the active handler (an unmount whose
 * successor already attached must not clear the live one). Events delivered
 * from here on are buffered by [`deliverAgentEvent`], not dropped.
 * Exported for tests.
 */
export function detachAgentDispatch(
  dispatch: (payload: AgentEventPayload) => void
): void {
  if (activeDispatch === dispatch) activeDispatch = null;
}

/**
 * Attach `dispatch` as the active handler and replay anything the listener
 * buffered while none was attached. Exported for tests.
 */
export function attachAgentDispatch(
  dispatch: (payload: AgentEventPayload) => void
): void {
  activeDispatch = dispatch;
  loggedMissingDispatch = false;
  if (pendingPayloads.length === 0) return;
  const replay = pendingPayloads.splice(0, pendingPayloads.length);
  for (const payload of replay) dispatch(payload);
}
/** Channel reported to the backend with the listener-registration diagnostic —
 *  the emit side logs the webview labels it emits to, so the two lines can be
 *  compared directly. Named AGENT_EVENT_CHANNEL to match the constants on
 *  both sides: tauri.ts (listen wrapper) and src-tauri/src/ipc/events.rs
 *  (emit side) (quality review Q4, 2026-09-08). */
const AGENT_EVENT_CHANNEL = "agent://event";
/** Whether the module-level Tauri listener has been started. */
let listenerStarted = false;
/** Whether the module-level backlog listener has been started. */
let backlogListenerStarted = false;
/** Whether the module-level browser-reveal listener has been started. */
let browserRevealListenerStarted = false;
/** Agent ids with an in-flight name-resolution `listAgents()` call — prevents
 *  one IPC per streamed token while the first round-trip is still pending. */
const nameResolutionInFlight = new Set<AgentId>();

/**
 * Module-level handle to the mounted hook's rAF delta buffers, so callers
 * outside the hook (e.g. the `/clear` slash command) can drain them. Without
 * this, a `text_delta` / `reasoning_delta` / `tool_call_arg_delta` /
 * `tool_output_delta` buffered before a clear would flush on the next
 * animation frame and repopulate the just-cleared streaming state.
 *
 * Registered by the hook's effect; `null` when no hook is mounted.
 */
export interface BufferHandle {
  /** The per-agent text buffer (mutated in place by the dispatcher). */
  textBuffers: Map<AgentId, string>;
  /** The per-agent reasoning buffer (mutated in place by the dispatcher). */
  reasoningBuffers: Map<AgentId, string>;
  /** Per-agent, per-call-index arg fragment buffers (mutated in place). */
  argBuffers: Map<AgentId, Map<number, string>>;
  /** Per-agent, per-tool-call-id live output buffers (mutated in place). */
  outputBuffers: Map<AgentId, Map<string, string>>;
  /** The pending rAF id ref (0 = no frame scheduled). */
  rafIdRef: { current: number };
  /** The pending fallback-flush timer ref (null = no timer armed). */
  timerIdRef: { current: ReturnType<typeof setTimeout> | null };
}
let activeBuffer: BufferHandle | null = null;

/** Cancel functions injected into [`drainStreamingBuffer`] so the drain logic
 *  is unit-testable in a node environment (no browser globals). */
export interface StreamCancelDeps {
  /** Cancel a scheduled rAF flush by id. */
  cancelFrame: (id: number) => void;
  /** Cancel an armed fallback-flush timer. */
  cancelTimer: (id: ReturnType<typeof setTimeout>) => void;
}

/**
 * Drain any buffered delta fragments for `id` (text, reasoning, arg and live
 * output deltas) from `handle`, and — once nothing remains buffered for ANY
 * agent — cancel the pending rAF flush + fallback timer via `cancel`, so a
 * just-cleared
 * conversation isn't repopulated by stale buffered deltas on the next
 * animation frame (or timer tick). When other agents still have buffered
 * deltas, the pending flush is left alone (it must still fire for them).
 * Pure function; [`clearStreamingBuffer`] is the mounted-hook wrapper.
 */
export function drainStreamingBuffer(
  handle: BufferHandle,
  id: AgentId,
  cancel: StreamCancelDeps,
): void {
  handle.textBuffers.delete(id);
  handle.reasoningBuffers.delete(id);
  handle.argBuffers.delete(id);
  handle.outputBuffers.delete(id);
  // If nothing remains buffered for any agent, cancel the pending flush so it
  // doesn't fire on the next frame with an empty batch (harmless, but tidy).
  const empty =
    handle.textBuffers.size === 0 &&
    handle.reasoningBuffers.size === 0 &&
    handle.argBuffers.size === 0 &&
    handle.outputBuffers.size === 0;
  if (empty) {
    if (handle.rafIdRef.current !== 0) {
      cancel.cancelFrame(handle.rafIdRef.current);
      handle.rafIdRef.current = 0;
    }
    if (handle.timerIdRef.current !== null) {
      cancel.cancelTimer(handle.timerIdRef.current);
      handle.timerIdRef.current = null;
    }
  }
}

/**
 * Drain the mounted hook's streaming-delta buffers for `id` and cancel the
 * pending rAF flush + fallback timer once nothing remains buffered, so a
 * just-cleared conversation isn't repopulated by stale buffered deltas on the
 * next animation frame (or timer tick). No-op when no hook is mounted or
 * nothing is buffered for the agent. Called by the `/clear` slash command
 * alongside `interrupt` + `clearConversation`.
 */
export function clearStreamingBuffer(id: AgentId): void {
  const handle = activeBuffer;
  if (!handle) return;
  drainStreamingBuffer(handle, id, {
    cancelFrame: (rafId) => cancelAnimationFrame(rafId),
    cancelTimer: (timerId) => clearTimeout(timerId),
  });
}

/**
 * Start the single Tauri agent-event listener (idempotent). Events are
 * forwarded to whatever handler the mounted hook has registered via
 * `activeDispatch`.
 */
function ensureListenerStarted(): void {
  if (listenerStarted) return;
  listenerStarted = true;
  // The returned unlisten promise is intentionally not awaited/used: the
  // listener lives for the whole window lifetime. (If we ever needed to tear
  // it down, we'd capture the promise here.)
  // The rejection MUST be surfaced: `listen()` is an async IPC call, and a
  // `void`-discarded rejection (a denied core:event permission, an IPC
  // failure) leaves the window with NO agent-event listener at all — every
  // Rust->TS event is then dropped with no error on either side, which looks
  // exactly like "the app works but the UI never updates".
  onAgentEvent((payload) => {
    deliverAgentEvent(payload);
  })
    .then(() => {
      void uiDiag(`agent-event listener registered for ${AGENT_EVENT_CHANNEL}`);
    })
    .catch((e) => {
      console.error(
        "FATAL: agent-event listener failed to register — the UI will never " +
          "update (no events can arrive):",
        e
      );
      void uiDiag(`FATAL agent-event listener registration failed: ${String(e)}`);
    });
}

/**
 * Start the single backlog listener (idempotent). Routes `backlog://changed`
 * events into the store via `applyBacklogChanged`. Lives at module scope for
 * the same StrictMode reason as the agent-event listener above.
 */
function ensureBacklogListenerStarted(): void {
  if (backlogListenerStarted) return;
  backlogListenerStarted = true;
  onBacklogChanged((payload) => {
    useAgentStore.getState().applyBacklogChanged(payload);
  }).catch((e) => {
    console.error("backlog listener failed to register:", e);
  });
}

/**
 * Start the single browser-reveal listener (idempotent). Routes
 * `browser://reveal` events (agent bootstrapped the child webview) into the
 * store via `handleBrowserReveal` — reveals + selects the Browser tab. Lives
 * at module scope for the same StrictMode reason as the listeners above.
 */
function ensureBrowserRevealListenerStarted(): void {
  if (browserRevealListenerStarted) return;
  browserRevealListenerStarted = true;
  onBrowserReveal(() => {
    handleBrowserReveal();
  }).catch((e) => {
    console.error("browser-reveal listener failed to register:", e);
  });
}

/** Dependencies injected into [`createAgentEventDispatcher`]: the buffer
 *  maps + rAF id holder (owned by the hook's refs), the frame functions,
 *  the fallback-timer + clock functions, and the store actions. Injected so
 *  tests can drive the dispatcher without React (review F6, 2026-08-18). */
export interface DispatcherDeps {
  /** The per-agent text buffer (mutated in place by the dispatcher). */
  textBuffers: Map<AgentId, string>;
  /** The per-agent reasoning buffer (mutated in place by the dispatcher). */
  reasoningBuffers: Map<AgentId, string>;
  /** Per-agent, per-call-index arg fragment buffers (mutated in place). */
  argBuffers: Map<AgentId, Map<number, string>>;
  /** Per-agent, per-tool-call-id live output buffers (mutated in place). */
  outputBuffers: Map<AgentId, Map<string, string>>;
  /** The pending rAF id holder (0 = no frame scheduled). */
  rafId: { current: number };
  /** The pending fallback-flush timer holder (null = no timer armed). */
  timerId: { current: ReturnType<typeof setTimeout> | null };
  /** `requestAnimationFrame` (injected for tests). */
  raf: (cb: () => void) => number;
  /** `cancelAnimationFrame` (injected for tests). */
  caf: (id: number) => void;
  /** `setTimeout` (injected for tests) — arms the fallback flush timer. */
  setTimer: (cb: () => void, ms: number) => ReturnType<typeof setTimeout>;
  /** `clearTimeout` (injected for tests) — disarms the fallback flush timer. */
  clearTimer: (id: ReturnType<typeof setTimeout>) => void;
  /** Monotonic-ish clock in ms (injected for tests) — drives the
   *  time-gap immediate-flush decision. */
  now: () => number;
  /** Store action: append a flushed text batch for `id`. */
  appendStreamingText: (id: AgentId, text: string) => void;
  /** Store action: append a flushed reasoning batch for `id`. */
  appendStreamingReasoning: (id: AgentId, text: string) => void;
  /** Store action: apply flushed tool-arg deltas for `id`. */
  applyToolCallArgDeltas: (
    id: AgentId,
    deltas: Array<{ index: number; fragment: string }>,
  ) => void;
  /** Store action: apply flushed `tool_output_delta` chunks for `id`. */
  applyToolOutputDeltas: (
    id: AgentId,
    deltas: Array<{ tool_call_id: string; text: string }>,
  ) => void;
  /** Store action: handle a non-delta event. */
  handleAgentEvent: (payload: AgentEventPayload) => void;
}

/** A dispatcher built by [`createAgentEventDispatcher`]. */
export interface AgentEventDispatcher {
  /** Dispatch one agent event: deltas (text, reasoning, tool-arg, live tool
   *  output) buffer until flushed per frame / per 64 KiB cap, everything else
   *  goes straight to the store. */
  dispatch: (payload: AgentEventPayload) => void;
  /** Flush all buffered deltas synchronously. Non-delta events flush
   *  automatically before dispatching; the hook also calls this on
   *  unmount so no buffered deltas are lost. */
  flush: () => void;
}

/**
 * Build the agent-event dispatcher: buffers `text_delta` / `reasoning_delta`
 * / `tool_call_arg_delta` / `tool_output_delta` fragments per agent and flushes
 * them to the store
 * once per animation frame (dense bursts), or immediately when a delta
 * arrives >= [`MAX_FLUSH_INTERVAL_MS`] after the last flush (sparse tokens —
 * the display streams per-token, independent of the frame loop), or
 * synchronously once a per-agent buffer passes the 64 KiB occlusion cap.
 * Each scheduled frame is paired with a one-shot fallback timer so a stalled
 * rAF (RDP occlusion / WebView2 quirks; rAF never fires while occluded —
 * R2 of .coding/reviews/2026-08-18-rdp-freeze-diagnosis.md, WebView2Feedback
 * #4879) can never hold text past [`MAX_FLUSH_INTERVAL_MS`]. Extracted from
 * the hook with injected dependencies so the buffering wiring is
 * unit-testable without React (review F6, 2026-08-18).
 */
export function createAgentEventDispatcher(deps: DispatcherDeps): AgentEventDispatcher {
  // Timestamp of the last flush to the store (on deps.now()'s clock).
  // Starts at 0 so the very first delta of the session flushes immediately —
  // the first token is visible as soon as it arrives, no frame required.
  let lastFlushAt = 0;

  /** Cancel any pending frame + fallback timer (no-ops when none pending). */
  const cancelPending = () => {
    if (deps.rafId.current !== 0) {
      deps.caf(deps.rafId.current);
      deps.rafId.current = 0;
    }
    if (deps.timerId.current !== null) {
      deps.clearTimer(deps.timerId.current);
      deps.timerId.current = null;
    }
  };

  /** Flush all buffered deltas (text, reasoning, tool-arg, live output) to the store in a single batch. */
  const flush = () => {
    cancelPending();
    lastFlushAt = deps.now();
    // Text
    const textBuffers = deps.textBuffers;
    if (textBuffers.size > 0) {
      const textEntries = Array.from(textBuffers.entries());
      textBuffers.clear();
      for (const [id, text] of textEntries) {
        deps.appendStreamingText(id, text);
      }
    }
    // Reasoning
    const reasoningBuffers = deps.reasoningBuffers;
    if (reasoningBuffers.size > 0) {
      const reasoningEntries = Array.from(reasoningBuffers.entries());
      reasoningBuffers.clear();
      for (const [id, text] of reasoningEntries) {
        deps.appendStreamingReasoning(id, text);
      }
    }
    // Tool arg deltas (per agent, per index). Convert to array-of-deltas form.
    const argBuffers = deps.argBuffers;
    if (argBuffers.size > 0) {
      for (const [id, idxMap] of Array.from(argBuffers.entries())) {
        if (idxMap.size === 0) continue;
        const deltas: Array<{ index: number; fragment: string }> = [];
        for (const [idx, frag] of idxMap.entries()) {
          if (frag.length > 0) deltas.push({ index: idx, fragment: frag });
        }
        idxMap.clear();
        if (deltas.length > 0) {
          deps.applyToolCallArgDeltas(id, deltas);
        }
      }
      // Drop any agent maps that became empty
      for (const [id, idxMap] of Array.from(argBuffers.entries())) {
        if (idxMap.size === 0) argBuffers.delete(id);
      }
    }
    // Live tool output (per agent, per tool-call id). Coalesced per call so a
    // chatty command's chunks cost one store write per frame instead of one
    // per chunk — the same treatment the tool-arg deltas get.
    const outputBuffers = deps.outputBuffers;
    if (outputBuffers.size > 0) {
      for (const [id, callMap] of Array.from(outputBuffers.entries())) {
        if (callMap.size === 0) continue;
        const deltas: Array<{ tool_call_id: string; text: string }> = [];
        for (const [callId, text] of callMap.entries()) {
          if (text.length > 0) deltas.push({ tool_call_id: callId, text });
        }
        callMap.clear();
        if (deltas.length > 0) {
          deps.applyToolOutputDeltas(id, deltas);
        }
      }
      // Drop any agent maps that became empty
      for (const [id, callMap] of Array.from(outputBuffers.entries())) {
        if (callMap.size === 0) outputBuffers.delete(id);
      }
    }
  };

  /** Schedule a flush on the next animation frame (if not already pending),
   *  plus a one-shot fallback timer: if the frame never fires (rAF stalls
   *  under RDP occlusion — WebView2Feedback #4879), the timer flushes at
   *  [`MAX_FLUSH_INTERVAL_MS`] instead of holding the text indefinitely.
   *  First-wins: the frame callback and the timer callback both route through
   *  `flush`, which cancels whichever side is still pending. */
  const scheduleFlush = () => {
    if (deps.rafId.current !== 0) return;
    deps.rafId.current = deps.raf(flush);
    if (deps.timerId.current === null) {
      deps.timerId.current = deps.setTimer(() => {
        deps.timerId.current = null;
        flush();
      }, MAX_FLUSH_INTERVAL_MS);
    }
  };

  /** Route a newly buffered delta to the store:
   *  - past the 64 KiB occlusion cap → flush synchronously (rAF never fires
   *    while occluded — R2 of .coding/reviews/2026-08-18-rdp-freeze-
   *    diagnosis.md);
   *  - >= [`MAX_FLUSH_INTERVAL_MS`] since the last flush → flush synchronously
   *    so sparse tokens (the norm for reasoning models) appear per-token even
   *    when the frame loop is stalled;
   *  - otherwise → batch and flush on the next frame (with the fallback
   *    timer guarding against a stalled frame loop). */
  const routeFlush = (bufferedChars: number) => {
    if (shouldFlushSync(bufferedChars) || shouldFlushNow(lastFlushAt, deps.now())) {
      flush();
    } else {
      scheduleFlush();
    }
  };

  /** Dispatch a payload to the store. Also resolves the display name for
   *  agents we haven't seen before (runtime-spawned via the spawn_agent
   *  tool): their tab would otherwise read "agent-<id>" until restart.
   *  Guarded by nameResolutionInFlight so a streaming unknown agent fires
   *  at most one listAgents() IPC per round-trip, not one per token. */
  const dispatch = (payload: AgentEventPayload) => {
    const st = useAgentStore.getState();
    if (
      st.agentNames[payload.agent_id] === undefined &&
      !nameResolutionInFlight.has(payload.agent_id)
    ) {
      nameResolutionInFlight.add(payload.agent_id);
      void listAgents()
        .then((infos) => useAgentStore.getState().registerAgents(infos))
        .catch((e) => console.error("failed to refresh agent names:", e))
        .finally(() => nameResolutionInFlight.delete(payload.agent_id));
    }
    if (payload.event.kind === "text_delta") {
      // Buffer the fragment — don't touch the store yet.
      const buffers = deps.textBuffers;
      const id = payload.agent_id;
      const prev = buffers.get(id) ?? "";
      const next = prev + payload.event.text;
      buffers.set(id, next);
      routeFlush(next.length);
    } else if (payload.event.kind === "reasoning_delta") {
      const buffers = deps.reasoningBuffers;
      const id = payload.agent_id;
      const prev = buffers.get(id) ?? "";
      const next = prev + payload.event.text;
      buffers.set(id, next);
      routeFlush(next.length);
    } else if (payload.event.kind === "tool_call_arg_delta") {
      const agentMap = deps.argBuffers;
      const id = payload.agent_id;
      let idxMap = agentMap.get(id);
      if (!idxMap) {
        idxMap = new Map<number, string>();
        agentMap.set(id, idxMap);
      }
      const prev = idxMap.get(payload.event.index) ?? "";
      const next = prev + payload.event.fragment;
      idxMap.set(payload.event.index, next);
      routeFlush(next.length);
    } else if (payload.event.kind === "tool_output_delta") {
      // Live tool output buffers like the other deltas: a chatty command can
      // emit 8 KiB chunks far faster than the shell's 60 ms throttle implies,
      // and every store write re-renders the transcript. Keyed by tool-call id
      // so one card's chunks coalesce into a single append per flush — and
      // since a `tool_result` flushes BEFORE it is dispatched, a buffered live
      // chunk can never land after the result it belongs to.
      const agentMap = deps.outputBuffers;
      const id = payload.agent_id;
      let callMap = agentMap.get(id);
      if (!callMap) {
        callMap = new Map<string, string>();
        agentMap.set(id, callMap);
      }
      const prev = callMap.get(payload.event.tool_call_id) ?? "";
      const next = prev + payload.event.text;
      callMap.set(payload.event.tool_call_id, next);
      routeFlush(next.length);
    } else {
      // Flush any pending deltas (text/reasoning/arg) synchronously before a
      // non-delta event so the store sees all accumulated content before the
      // next structural change (tool card, result, approval, etc.).
      flush();

      // Auto-reveal the plan on plan activity, mirroring how the DiffViewer
      // surfaces itself. Only when the user hasn't manually hidden the panel
      // (autoRevealPlan no-ops when the panel is visible or Plan is disabled).
      const evt = payload.event;
      if (evt.kind === "workflow_state_changed") {
        // Planning → Executing = a plan was just created. Read the previous
        // phase from the store BEFORE dispatching (dispatch updates it).
        const prev = useAgentStore.getState().workflowStates[payload.agent_id];
        if (evt.state === "executing" && prev !== "executing") {
          useAgentStore.getState().autoRevealPlan();
        }
      } else if (evt.kind === "step_completed") {
        useAgentStore.getState().autoRevealPlan();
      } else if (evt.kind === "approval_request") {
        // Auto-reveal the Diff viewer for file tool approvals
        // (file_edit / file_write / file_append). Mirrors the plan
        // auto-reveal behavior: only when panel is hidden and the tab is
        // enabled; never yank the user from a visible panel.
        const tool = evt.tool_name;
        if (tool === "file_edit" || tool === "file_write" || tool === "file_append") {
          useAgentStore.getState().autoRevealDiff();
        }
        // Auto-switch to a subagent when it hits a security approval, so the
        // user sees the approval prompt without hunting for the right tab.
        // Only subagents (those with a recorded parent) auto-switch — the
        // main agent keeps the existing "Switch & review" banner (it's the
        // primary surface the user is already watching). Never switch to an
        // agent that's already active (no-op) and never steal focus back to
        // a subagent the user navigated away from for a *second* approval in
        // the same turn (the first switch already surfaced it).
        const st = useAgentStore.getState();
        const isSubagent = st.agentParents[payload.agent_id] != null;
        if (isSubagent && st.activeAgent !== payload.agent_id) {
          st.setActiveAgent(payload.agent_id);
        }
      } else if (evt.kind === "user_question") {
        // Auto-switch to a subagent when it asks a question, so the user
        // sees the question prompt without hunting for the right tab
        // (mirrors the approval_request auto-switch). The main agent is the
        // primary surface the user is already watching, so it never
        // auto-switches.
        const st = useAgentStore.getState();
        const isSubagent = st.agentParents[payload.agent_id] != null;
        if (isSubagent && st.activeAgent !== payload.agent_id) {
          st.setActiveAgent(payload.agent_id);
        }
      }

      deps.handleAgentEvent(payload);
    }
  };

  return { dispatch, flush };
}

export function useAgentEvents() {
  const handleAgentEvent = useAgentStore((s) => s.handleAgentEvent);
  const appendStreamingText = useAgentStore((s) => s.appendStreamingText);
  const appendStreamingReasoning = useAgentStore((s) => s.appendStreamingReasoning);
  const applyToolCallArgDeltas = useAgentStore((s) => s.applyToolCallArgDeltas);
  const applyToolOutputDeltas = useAgentStore((s) => s.applyToolOutputDeltas);

  // Per-agent text-delta buffer. Keyed by agent id so multiple agents don't
  // clobber each other's buffered text.
  const textBuffersRef = useRef<Map<AgentId, string>>(new Map());
  // Per-agent reasoning-delta buffer.
  const reasoningBuffersRef = useRef<Map<AgentId, string>>(new Map());
  // Per-agent, per-call-index arg-delta buffers: agent -> (index -> fragment)
  const argBuffersRef = useRef<Map<AgentId, Map<number, string>>>(new Map());
  // Per-agent, per-call-id live output buffers: agent -> (call id -> text)
  const outputBuffersRef = useRef<Map<AgentId, Map<string, string>>>(new Map());
  // The pending rAF id (0 = no frame scheduled).
  const rafIdRef = useRef<number>(0);
  // The pending fallback-flush timer (null = no timer armed).
  const timerIdRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    // The dispatcher owns the buffering/flush policy; the hook injects its
    // buffer refs + the store actions (see createAgentEventDispatcher).
    const dispatcher = createAgentEventDispatcher({
      textBuffers: textBuffersRef.current,
      reasoningBuffers: reasoningBuffersRef.current,
      argBuffers: argBuffersRef.current,
      outputBuffers: outputBuffersRef.current,
      rafId: rafIdRef,
      timerId: timerIdRef,
      raf: requestAnimationFrame,
      caf: cancelAnimationFrame,
      setTimer: (cb, ms) => setTimeout(cb, ms),
      clearTimer: (id) => clearTimeout(id),
      now: () => Date.now(),
      appendStreamingText,
      appendStreamingReasoning,
      applyToolCallArgDeltas,
      applyToolOutputDeltas,
      handleAgentEvent,
    });

    // Register this instance's dispatch as the active handler and make sure
    // the single module-level Tauri listener exists. A re-mount (StrictMode)
    // simply swaps `activeDispatch` — no second listener is ever opened.
    // Attaches AND replays anything the listener buffered while no
    // dispatcher was mounted — events are never dropped in the gap.
    attachAgentDispatch(dispatcher.dispatch);
    // Register the rAF buffers so `clearStreamingBuffer` (used by `/clear`)
    // can drain stale delta fragments (text, reasoning, arg) before they
    // repopulate a cleared conversation. Re-registered each mount; cleared on
    // unmount below.
    activeBuffer = {
      textBuffers: textBuffersRef.current,
      reasoningBuffers: reasoningBuffersRef.current,
      argBuffers: argBuffersRef.current,
      outputBuffers: outputBuffersRef.current,
      rafIdRef,
      timerIdRef,
    };
    ensureListenerStarted();
    ensureBacklogListenerStarted();
    ensureBrowserRevealListenerStarted();

    // Initial backlog fetch (populates the store before the first
    // `backlog://changed` event arrives).
    backlogList()
      .then((items) => useAgentStore.getState().setBacklog(items))
      .catch((e) => console.error("failed to list backlog:", e));

    return () => {
      // Detach this instance's handler if it's still the active one. We do
      // NOT tear down the module-level Tauri listener — it persists across
      // the StrictMode remount so no events are dropped in the gap.
      detachAgentDispatch(dispatcher.dispatch);
      // Detach the rAF buffer handle if it's still ours.
      if (
        activeBuffer?.textBuffers === textBuffersRef.current &&
        activeBuffer?.reasoningBuffers === reasoningBuffersRef.current &&
        activeBuffer?.argBuffers === argBuffersRef.current &&
        activeBuffer?.outputBuffers === outputBuffersRef.current
      ) {
        activeBuffer = null;
      }
      // Final synchronous flush so no buffered deltas are lost on unmount —
      // it cancels any pending frame and fallback timer itself.
      dispatcher.flush();
    };
  }, [
    handleAgentEvent,
    appendStreamingText,
    appendStreamingReasoning,
    applyToolCallArgDeltas,
    applyToolOutputDeltas,
  ]);
}
