// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Pure per-event reducers for the frontend agent store.
//
// Extracted from `useAgentStore.ts` (Maint H3): each `SerializableAgentEvent`
// kind has a pure reducer `(agent, event) => ReducerResult` that returns the
// new per-agent state plus optional cross-cutting `Effects`. `applyAgentEvent`
// dispatches a single event through the uniform write path and returns the
// new `AppState` (plus effects for the caller to apply — e.g. the
// landed-steer removal timer). Everything here is pure (no store, no DOM,
// no timers) so the table-driven vitest suite can exercise every event kind.

import type {
  AgentId,
  SerializableAgentEvent,
  TranscriptEntry,
  WorkflowState,
} from "../lib/types";
import type { SoundKind } from "../lib/sounds";
import { fmtTokens } from "../lib/format";
import { isBrowserToolName } from "../lib/toolCardPaths";

import type {
  ActivityEntry,
  AgentState,
  LastDiff,
} from "./agentState";
import {
  RECENT_TIMING_WINDOW,
  allocEntryId,
  capActivityLog,
  capTranscript,
  flushStreamingText,
  getOrCreate,
  pushTranscriptEntry,
  selectMainAgentId,
} from "./agentState";

/**
 * The slice of AppState the reducer dispatcher touches. The store facade
 * (`useAgentStore.ts`) defines the full `AppState` interface and its `create`
 * satisfies this shape structurally.
 */
export interface AppStateLike {
  agents: Record<AgentId, AgentState>;
  agentNames: Record<AgentId, string>;
  agentParents: Record<AgentId, AgentId | null>;
  agentModels: Record<AgentId, string>;
  /** Per-agent serving endpoint name (from AgentInfo.provider /
   *  ModelChanged.provider), keyed by id — the backend names WHICH endpoint
   *  serves the agent's model, so the status bar never mislabels the
   *  provider when the same model id is listed under two endpoints. Absent
   *  for an agent whose endpoint is unknown; the UI then resolves the model
   *  id against endpoints.toml. */
  agentProviders: Record<AgentId, string>;
  /** Per-agent reasoning effort last chosen in the toolbar, keyed by agent id
   *  (models are agent-specific, so effort is too: each agent's provider was
   *  rebuilt with its own effort). Absent for an agent that was never
   *  switched — the UI falls back to its endpoint's configured default. */
  agentEfforts: Record<AgentId, string>;
  /** Per-agent WIRE-reported effective reasoning effort (from
   *  AgentInfo.reasoning_effort / ModelChanged.reasoning_effort), keyed by
   *  agent id — the backend's resolution of the model actually serving the
   *  agent's current context, in UI vocabulary. The status bar prefers this
   *  over the toolbar echo / endpoint default (backlog 51dab4da). Absent
   *  when the backend doesn't know (mock-backed loops). */
  agentWireEfforts: Record<AgentId, string>;
  activeAgent: AgentId | null;
  workflowStates: Record<AgentId, WorkflowState>;
  lastDiff: LastDiff | null;
  /** Captured diffs of the current top-level plan, newest first, one per path. */
  planDiffs: LastDiff[];
  /** The root plan id from workflow_state_changed (null when no plan). */
  topPlanId: string | null;
  /** The user's explicit Diff-tab dropdown pick; null = follow latest/pending. */
  selectedDiffPath: string | null;
  planVersion: number;
}

/**
 * How long a landed steer stays in the backlog before auto-removing (ms).
 * Gives the user time to see the "steer landed" highlight before it fades.
 */
export const STEER_LANDED_TTL_MS = 5000;

/**
 * Cap on merged same-name calls within ONE tool transcript entry (review M2,
 * 2026-04-19 freeze-fix review): the merge path appended every consecutive
 * call to the same card and tool_result kept that card "last" (updating it in
 * place), so one entry's `calls` array — each with full args + output — grew
 * without bound while MAX_TRANSCRIPT_ENTRIES never fired (entry count stays
 * 1). Past the cap a NEW card is pushed instead of merging.
 */
export const MAX_CALLS_PER_TOOL_CARD = 50;

type Ev<K extends SerializableAgentEvent["kind"]> =
  Extract<SerializableAgentEvent, { kind: K }>;

/**
 * Whether a tool-result error string is a user denial / interrupt rather
 * than a model/tool failure. Mirrors the backend's
 * `is_user_denial_tool_output` (src/agent/turn.rs) EXACTLY — same
 * substrings — so the frontend doom streak excludes deliberate safety
 * choices (DenyAll batches, interrupts) just like the backend's
 * MAX_RETRIES counter does (review finding 2). Pure; pinned by test.
 */
export function isUserDenialToolOutput(output: string): boolean {
  return (
    output.includes("user denied") ||
    output.includes("interrupted while awaiting approval") ||
    output.includes("cancelled while awaiting approval") ||
    output.includes("approval channel closed") ||
    output.includes("interrupted: not run")
  );
}

/**
 * How many consecutive errors (failed tool results and/or error events) must
 * pile up before a FINAL error (the agent stopping) triggers the doom sound.
 * Mirrors the backend `agent::MAX_RETRIES` consecutive-tool-error cap, but
 * also covers the provider-retry path (its retrying errors are part of the
 * same user-visible "three errors and it stopped" experience).
 */
export const DOOM_ERROR_STREAK = 3;

/**
 * Cross-cutting effects a reducer may produce alongside the new per-agent
 * state. The dispatcher applies these after computing the `agents` map.
 */
export interface Effects {
  /** Replace the last-diff snapshot (null = clear). */
  lastDiff?: LastDiff | null;
  /** Upsert into the current top plan's changed-file list (one entry per path). */
  planDiff?: LastDiff;
  /** Bump planVersion (plan changed — PlanProgress re-fetches). */
  planVersionBump?: boolean;
  /** Merge into workflowStates. */
  workflowStates?: Record<AgentId, WorkflowState>;
  /** Merge into agentNames. */
  agentNames?: Record<AgentId, string>;
  /** Merge into agentModels (the per-agent model id map). */
  agentModels?: Record<AgentId, string>;
  /** Merge into agentProviders (the per-agent serving-endpoint map): a
   *  non-empty name upserts, `null`/empty CLEARS the agent's entry (the UI
   *  falls back to resolving the model id against endpoints.toml). */
  agentProviders?: Record<AgentId, string | null>;
  /** Merge into agentWireEfforts (the per-agent WIRE-reported effective
   *  effort map, backlog 51dab4da): a non-empty value upserts, `null`/empty
   *  CLEARS the agent's entry (the UI falls back to the toolbar echo /
   *  endpoint default — never label the OLD effort against the NEW model). */
  agentWireEfforts?: Record<AgentId, string | null>;
  /**
   * Remove the agent entirely (exited): delete it from agents/agentNames/
   * agentParents/workflowStates, and fall back to the main agent when the
   * removed agent was active.
   */
  removeAgent?: boolean;
  /**
   * Schedule the auto-removal of a landed steer after STEER_LANDED_TTL_MS.
   * (The reducer is pure — the dispatcher owns this setTimeout.)
   */
  scheduleSteerRemoval?: { agentId: AgentId; steerId: number };
  /**
   * Play a notification sound (complete ding / needs-input ping / doom).
   * The reducer stays pure — the dispatcher executes this through
   * `playSound`, gated by the store's per-sound enable flags.
   */
  sound?: SoundKind;
}

export type ReducerResult = { agent: AgentState; effects?: Effects };
export type Reducer<E> = (agent: AgentState, event: E) => ReducerResult;

/**
 * Cap on the current top plan's changed-file list (review L1, 2026-04-19
 * freeze-fix review): `planDiffs` keeps one entry per distinct edited path —
 * each holding a full diff body — and previously grew without bound for the
 * life of the plan (it only reset when `topPlanId` changed). The list is
 * newest-first, so the cap keeps the most recent paths and drops the oldest.
 */
export const MAX_PLAN_DIFFS = 100;

/**
 * Upsert a captured diff into a top plan's changed-file list: one entry per
 * path, newest first. A re-edit of the same path replaces its earlier entry
 * (the latest snapshot wins) and moves it to the front. Capped at
 * [`MAX_PLAN_DIFFS`] — the oldest paths drop off the end.
 */
export function upsertPlanDiff(list: LastDiff[], d: LastDiff): LastDiff[] {
  return [d, ...list.filter((e) => e.path !== d.path)].slice(0, MAX_PLAN_DIFFS);
}

/** started: reset the per-turn state (fresh run). Clears any pending
 *  question — the agent resumed (with the answer), so the question prompt is
 *  no longer pending. Also clears the answered marker + freeform-answer mode
 *  (the question was fully resolved). (pendingApproval is cleared on
 *  tool_result, not here.) */
export const reduceStarted: Reducer<Ev<"started">> = (agent) => ({
  agent: {
    ...agent,
    running: true,
    streamingText: "",
    streamingReasoning: "",
    activityLog: [],
    tokenUsage: { prompt: 0, completion: 0, reasoning: 0, cached: 0 },
    lastRequestTiming: null,
    contextUsage: { used: 0, max: 0 },
    pendingQuestion: null,
    pendingQuestionAnswered: null,
    freeformQuestionId: null,
    // A (re-)Started always means the backend is building/retrying the
    // provider request — flip the inflight bar to "sending" (the backend's
    // own Phase events refine it from here).
    phase: "sending",
    // A fresh run (or retry) is not a failed state — clear the Continue
    // affordance so it only shows after a FINAL error.
    failed: false,
    // A fresh run clears the parked banner — the agent is working again.
    parked: null,
    // The elapsed timer starts only when THIS Started begins a NEW turn
    // (idle → running). Provider retries re-emit Started between attempts
    // while the agent never stopped running, so resetting unconditionally
    // would restart the clock mid-turn (same reasoning as the
    // consecutiveToolErrors reset below).
    turnStartedAt: agent.running ? agent.turnStartedAt : Date.now(),
    liveCompletionTokens: 0,
    liveReasoningTokens: 0,
    // Reset the doom streak only when this Started begins a NEW turn (the
    // agent was idle). Provider retries re-emit Started between attempts
    // while the agent never stopped running (a retrying error keeps
    // running=true and no Finished is emitted until the terminal event), so
    // resetting unconditionally wiped the streak the final exhaustion error
    // needs to reach DOOM_ERROR_STREAK (review finding 1). Fresh turns DO
    // reset — they always follow a Finished (running=false).
    consecutiveToolErrors: agent.running ? agent.consecutiveToolErrors : 0,
  },
});

/**
 * phase: the backend's authoritative request-loop phase — flips the inflight
 * bar's status label (sending → waiting → reasoning → answering → running
 * tools). The frontend overrides the label locally for approval/question
 * pauses without touching this value.
 */
export const reducePhase: Reducer<Ev<"phase">> = (agent, event) => ({
  agent: { ...agent, phase: event.phase },
});

/**
 * Append a streamed answer chunk to the activity log, coalescing into the
 * last `answer` entry when one is already tailing (the reasoning box shows
 * the live completion as it streams — user request 2026-08-22). Pure —
 * returns the new array; a no-op for empty chunks.
 */
export function appendAnswerEntry(activityLog: ActivityEntry[], text: string): ActivityEntry[] {
  if (text.length === 0) return activityLog;
  const last = activityLog[activityLog.length - 1];
  if (last && last.kind === "answer") {
    const next = [...activityLog];
    next[next.length - 1] = { ...last, text: last.text + text };
    return capActivityLog(next);
  }
  return capActivityLog([
    ...activityLog,
    { kind: "answer" as const, text, timestamp: Date.now() },
  ]);
}

/** text_delta: append to the in-flight streaming text.
 *  O(current length) per flush — one concat per rAF frame (not per token),
 *  mitigated by rAF batching + JS string ropes. O(n²) total for a full
 *  response is accepted (micro-opt; the single hottest allocation path).
 *
 *  Also mirrors the chunk into the activity log via [`appendAnswerEntry`] —
 *  the completion is visible in the reasoning box as it streams; a following
 *  reasoning_delta replaces the log per the existing rule. */
export const reduceTextDelta: Reducer<Ev<"text_delta">> = (agent, event) => ({
  agent: {
    ...agent,
    streamingText: agent.streamingText + event.text,
    activityLog: appendAnswerEntry(agent.activityLog, event.text),
    // Live tokens-received estimate (chars/4 — the same heuristic the
    // backend uses for incremental token counting), completion bucket. Snaps
    // to the real count on the next usage event.
    liveCompletionTokens:
      agent.liveCompletionTokens + Math.max(1, Math.round(event.text.length / 4)),
  },
});

/**
 * reasoning_delta: append to the streaming reasoning + the last reasoning
 * activity-log entry. When new reasoning starts (last log entry isn't
 * reasoning), clear the log so the previous turn's reasoning is replaced.
 * The previous reasoning stays visible until this moment — not cleared when
 * the response lands.
 */
export const reduceReasoningDelta: Reducer<Ev<"reasoning_delta">> = (agent, event) => {
  const streamingReasoning = agent.streamingReasoning + event.text;
  let activityLog: ActivityEntry[];
  if (
    agent.activityLog.length === 0 ||
    agent.activityLog[agent.activityLog.length - 1].kind !== "reasoning"
  ) {
    activityLog = [
      { kind: "reasoning" as const, text: event.text, timestamp: Date.now() },
    ];
  } else {
    // Append to the last reasoning entry.
    const log = [...agent.activityLog];
    log[log.length - 1] = {
      ...log[log.length - 1],
      text: log[log.length - 1].text + event.text,
    };
    activityLog = log;
  }
  // Cap is a no-op today (both branches above never grow the array), kept as
  // a guard so future edits to this reducer can't reintroduce unbounded log
  // growth (F3).
  return {
    agent: {
      ...agent,
      streamingReasoning,
      activityLog: capActivityLog(activityLog),
      // Reasoning deltas are received tokens too — their own live bucket, so
      // the 🧠 counter climbs during the (often longest) reasoning phase
      // instead of sitting at 0 until the usage event lands.
      liveReasoningTokens:
        agent.liveReasoningTokens + Math.max(1, Math.round(event.text.length / 4)),
    },
  };
};

/**
 * The memory tools. These render as a dedicated readable `memory` entry
 * (tier/title/snippet) instead of a generic ToolCard. Visibility is a
 * render-time concern: the entry always enters the transcript store, and
 * Conversation.tsx hides it (with every other activity card) when
 * `show_tool_activity` is off — the Output tab still logs every tool result
 * regardless.
 */
const MEMORY_TOOLS = new Set([
  "memory_write",
  "memory_search",
  "memory_consolidate",
  // Phase 1 hygiene tools + Phase 3 retrieval tools — same bookkeeping
  // posture (they only touch the project's own memory store).
  "memory_update",
  "memory_supersede",
  "memory_delete",
]);

/**
 * Parse a memory tool's result into the tier/title/snippet shown on a `memory`
 * transcript entry — everything comes from the RESULT OUTPUT (this parser has
 * no access to the call's args). For `memory_write` that's the confirmation
 * sentence: the current `Saved {phrase} "{title}" ({tier} memory)[ — {suffix}]`
 * format first, with a legacy `wrote {tier} memory (id: …)` fallback (the
 * legacy form carries no title). For `memory_search` the tier/title come from
 * the first `[tier] title (id: …, score: …)` line — the stable id has been the
 * first parenthesized field since Phase 1 (2026-08-22); legacy `(score: …)`
 * lines (older logs) still parse. The snippet is a 160-char slice of
 * the result output for write/consolidate (the confirmation/summary sentence)
 * and the recalled content line for recall. For `memory_search` the full
 * per-hit list is also parsed (backlog 41f39672) so the activity card can
 * expand every matched hit rather than only the count. Query-less BROWSE
 * results (backlog aec6cc8a) carry no brackets or scores — tier/title then
 * come from the first `{uuid}  {tier}  {record_type}  {date}  {title}` row,
 * every row becomes a score-less hit, and the browse headers ("N memories
 * (newest first):" / "no memories match the filter") feed the same matched
 * chip.
 */
export function parseMemoryEntry(
  name: string,
  result: { success: boolean; output: string },
): {
  tier?: string;
  title?: string;
  snippet?: string;
  matched?: number;
  hits?: { tier: string; title: string; score?: number }[];
} {
  const out = result.output ?? "";
  if (name === "memory_write") {
    // New format (backlog 2026-08-20): 'Saved {phrase} "{title}" ({tier}
    // memory)[ — {suffix}]' — a human sentence naming what was saved. The
    // write id lives in structured `data`, not the text.
    //
    // ONE boundary-anchored pattern (review L2): the greedy title capture
    // runs to the LAST `" (tier memory)` marker followed by ` — ` or
    // end-of-string, so a title containing double quotes — or a fake
    // `(tier memory)` fragment before the real marker — parses correctly.
    // Separate first-match regexes silently mislabeled such titles.
    const nm = out.match(
      /Saved [^"]*?"(.+)" \((working|episodic|semantic|procedural) memory\)(?: — |$)/i,
    );
    if (nm) {
      return {
        tier: nm[2]?.toLowerCase(),
        title: nm[1],
        snippet: out.slice(0, 160) || undefined,
      };
    }
    // Legacy format fallback: "wrote {tier} memory (id: …)" — kept so old
    // logs / older binaries still parse a tier.
    const m = out.match(/wrote (\w+) memory/i);
    const tier = m?.[1]?.toLowerCase();
    // Title/content aren't in the result; leave them undefined (the call's
    // args had them, but the result only confirms the write). We surface the
    // tier + a short result snippet.
    return { tier, snippet: out.slice(0, 160) || undefined };
  }
  if (name === "memory_search") {
    // The recall result is "N memories matched:\n\n[tier] title (id: …,
    // score: …, strength: …)\n  content…". The first "\n\n"-delimited block is
    // the header line, so we match the first [tier] line directly across the
    // whole output (skipping the header) and pull the indented content line
    // that follows it. The stable id has been the FIRST parenthesized field
    // since Phase 1 (2026-08-22); the optional group keeps legacy
    // "(score: …)" lines (older logs / older binaries) parsing. The Phase-3
    // `memory_search` covers what memory_recall / plans_search /
    // reviews_search / past_fixes / context_pack used to emit separately —
    // one line shape for all of them, so one branch parses the lot.
    const tm = out.match(/\[(\w+)\]\s+(.+?)\s+\((?:id: [^,)]+, )?score:/);
    let tier = tm?.[1]?.toLowerCase();
    let title = tm?.[2];
    // The content line is the indented line right after the matched title line.
    // Search for a newline followed by whitespace + non-empty text after the
    // match position.
    const afterMatch = tm ? out.slice(tm.index! + tm[0].length) : "";
    const cm = afterMatch.match(/\n\s+(.+)/);
    const snippet = cm?.[1]?.slice(0, 160);
    // The full per-hit list for the card's expandable row (backlog 41f39672
    // — "12 matched, alas no expanding to see the 12") — same line pattern
    // as the first-hit match above, but global: every [tier] row becomes a
    // hit with its score. The backend's search-mode cap fits a full DEFAULT
    // page (limit 12), so nothing is truncated for default searches;
    // explicit larger limits may still truncate, dropping trailing rows
    // from this list while the card's matched chip keeps the query's count.
    const hitRe = /^\[(\w+)\]\s+(.+?)\s+\((?:id: [^,)]+, )?score: (\d+(?:\.\d+)?)/gm;
    const hits: { tier: string; title: string; score?: number }[] = [];
    let hmExec: RegExpExecArray | null;
    while ((hmExec = hitRe.exec(out)) !== null) {
      hits.push({
        tier: hmExec[1].toLowerCase(),
        title: hmExec[2],
        score: hmExec[3] !== undefined ? parseFloat(hmExec[3]) : undefined,
      });
    }
    // BROWSE rows (backlog aec6cc8a — the no-query emitter shape):
    // "{id}  {tier}  {record_type}  {date}  {title}[ [superseded]]" — no
    // brackets, no score. Parsed only when the bracket pass found nothing
    // (an output is one shape or the other): the first row feeds
    // tier/title, every row becomes an expandable hit WITHOUT a score (the
    // card renders the score column only when present).
    if (hits.length === 0) {
      const browseRe =
        /^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})  (\w+)  (\w+)  (\d{4}-\d{2}-\d{2})  (.+?)(?: \[superseded\])?$/gm;
      let bm: RegExpExecArray | null;
      while ((bm = browseRe.exec(out)) !== null) {
        if (tier === undefined) {
          tier = bm[2].toLowerCase();
          title = bm[5];
        }
        hits.push({ tier: bm[2].toLowerCase(), title: bm[5] });
      }
    }
    // The header line carries the match count. The Rust emitters
    // (src/tool/memory/retrieval.rs) produce: SEARCH — "N memories
    // matched:" / "no memories matched the query"; BROWSE (no query) —
    // "N memories (newest first):" / "no memories match the filter"
    // (backlog aec6cc8a: browse headers feed the same chip; anchors to the
    // line start only, so trailing prose is tolerated). Parsed for the
    // card's "N matched" / "no matches" chip.
    const hm = out.match(
      /^(?:(\d+) \w+|no \w+) matched:?|^(\d+) memories \(newest first\):?|^no \w+ match the filter/m,
    );
    const matched = hm
      ? hm[1] !== undefined
        ? parseInt(hm[1], 10)
        : hm[2] !== undefined
          ? parseInt(hm[2], 10)
          : 0
      : undefined;
    return { tier, title, snippet, matched, hits };
  }
  // memory_consolidate — the result is a one-line summary; use it as the snippet.
  return { snippet: out.slice(0, 160) || undefined };
}

/**
 * tool_call_start: flush pre-tool text, then merge consecutive calls to the
 * same tool into one card (append to the last entry when it's a tool with
 * the same name), else push a new tool transcript entry. Shell, browser
 * (`browser_*`/`offscreen_browser_*`), and search (`search`/`search_read`)
 * calls never merge — each starts its own card (backlog daa38cbe,
 * 24ddb845).
 */
export const reduceToolCallStart: Reducer<Ev<"tool_call_start">> = (agent, event) => {
  const next = { ...agent };
  // Flush any text the model streamed before this tool call — it belongs
  // *above* the tool card, not below it. If there was text, it becomes an
  // assistant entry, which prevents merging (text between tools → separate
  // cards).
  flushStreamingText(next);
  // Memory tools always push a readable `memory` entry (never a generic
  // ToolCard). Visibility is a render-time concern — Conversation.tsx hides
  // activity cards when show_tool_activity is off; the tool still runs and its
  // result still lands in the Output tab via reduceToolResult's
  // toolOutputEntry effect.
  if (MEMORY_TOOLS.has(event.name)) {
    const transcript = [...next.transcript];
    transcript.push({
      kind: "memory",
      name: event.name,
      // Correlation keys: `id` lets tool_result finalize exactly this entry
      // (a foreign tool's result must never steal it — regression pin in
      // useAgentStore.test.ts), `index` routes arg deltas into `args`.
      id: event.id,
      index: event.index,
      args: "",
      tier: undefined,
      title: undefined,
      snippet: undefined,
      success: false,
      running: true,
    });
    next.transcript = capTranscript(transcript);
    return { agent: next };
  }
  // Merge consecutive calls to the same tool into one card: if the last
  // transcript entry is a tool with the same name, append to it. This covers
  // both sequential calls (call→result→call) and parallel calls (multiple
  // tool_call_start in one response). The only things that break the chain
  // are intervening text (handled by the flush above), a different tool, or
  // the shell/browser exception below.
  // Past MAX_CALLS_PER_TOOL_CARD a fresh card starts (review M2: otherwise
  // one card's calls array grows without bound in long same-tool runs).
  const transcript = [...next.transcript];
  const last = transcript[transcript.length - 1];
  // A failed call breaks the merge chain: if the last call in the existing
  // card already completed with an error, start a fresh card so the error and
  // the retry (which may succeed) are never combined into one card. A call
  // still running (result === null) or a successful call does NOT break the
  // chain — parallel calls in the same batch still merge.
  const lastCall = last && last.kind === "tool" ? last.calls[last.calls.length - 1] : undefined;
  const lastCallFailed =
    lastCall !== undefined &&
    lastCall.result !== null &&
    !lastCall.result.success;
  // Shell commands, browser actions (live-tab browser_* + headless
  // offscreen_browser_*), and search queries (search/search_read) never
  // merge: the command/target/query of each call is the point of the card,
  // and grouped chips bury it — every such call renders on its own line
  // (user directive, backlog daa38cbe + 24ddb845).
  const neverGroups =
    event.name === "shell" ||
    event.name === "search" ||
    event.name === "search_read" ||
    isBrowserToolName(event.name);
  const canMerge =
    !neverGroups &&
    last !== undefined &&
    last.kind === "tool" &&
    last.name === event.name &&
    !lastCallFailed &&
    last.calls.length < MAX_CALLS_PER_TOOL_CARD;
  if (canMerge) {
    transcript[transcript.length - 1] = {
      ...last,
      calls: [
        ...last.calls,
        {
          id: event.id,
          index: event.index,
          args: "",
          result: null,
          // Timing stamp (tool-card duration display): when this call was
          // issued. tool_result stamps the matching endedAt.
          startedAt: Date.now(),
        },
      ],
    };
  } else {
    transcript.push({
      kind: "tool",
      name: event.name,
      calls: [
        {
          id: event.id,
          index: event.index,
          args: "",
          result: null,
          startedAt: Date.now(),
        },
      ],
    });
  }
  next.transcript = capTranscript(transcript);
  return { agent: next };
};

/**
 * tool_call_arg_delta: accumulate an argument fragment into the in-flight
 * call (result null) of the matching tool entry.
 */
export const reduceToolCallArgDelta: Reducer<Ev<"tool_call_arg_delta">> = (
  agent,
  event,
) => {
  const transcript = [...agent.transcript];
  for (let i = transcript.length - 1; i >= 0; i--) {
    const entry = transcript[i];
    if (entry.kind === "tool") {
      const callIdx = entry.calls.findIndex(
        (c) => c.index === event.index && c.result === null,
      );
      if (callIdx !== -1) {
        const calls = [...entry.calls];
        calls[callIdx] = {
          ...calls[callIdx],
          args: calls[callIdx].args + event.fragment,
        };
        transcript[i] = { ...entry, calls };
        break;
      }
    } else if (entry.kind === "memory") {
      // Memory entries accumulate args too (matched by the owning call's
      // index while running) so the card can show WHAT was searched —
      // the memory_search query chip parses these args at render time.
      if (entry.running && entry.index === event.index) {
        transcript[i] = { ...entry, args: (entry.args ?? "") + event.fragment };
        break;
      }
    }
  }
  return { agent: { ...agent, transcript } };
};

/**
 * Cap on the live output tail kept per running tool call, in chars. The card
 * renders only the last few lines and the FULL text arrives with the result,
 * so this bounds the per-call footprint of a very chatty command (`cargo
 * build` can emit megabytes) without affecting what the model receives.
 */
export const LIVE_OUTPUT_CAP = 16 * 1024;

/**
 * Append a live output chunk to the kept tail, dropping the head past
 * [`LIVE_OUTPUT_CAP`]. Exported so the batched store action
 * (`applyToolOutputDeltas`) applies the identical policy.
 *
 * The head-drop cuts on a UTF-16 code-unit boundary, which can split an astral
 * char (an emoji in compiler output) into a lone surrogate — that renders as a
 * replacement glyph, so a leading orphan half is dropped too.
 */
export function appendLiveOutputTail(prev: string | undefined, text: string): string {
  const joined = (prev ?? "") + text;
  if (joined.length <= LIVE_OUTPUT_CAP) return joined;
  const kept = joined.slice(-LIVE_OUTPUT_CAP);
  const first = kept.charCodeAt(0);
  return first >= 0xdc00 && first <= 0xdfff ? kept.slice(1) : kept;
}

/**
 * tool_output_delta: append a live output chunk to the RUNNING call with this
 * id, keeping only the tail ([`LIVE_OUTPUT_CAP`]).
 *
 * Returns the agent UNCHANGED when no running call carries the id: a chunk can
 * lose the race against its own result (a timed-out or cancelled reader emits
 * after the fact), and such a late chunk must be ignored rather than
 * resurrecting — or writing into — a card that already has its result.
 */
export const reduceToolOutputDelta: Reducer<Ev<"tool_output_delta">> = (
  agent,
  event,
) => {
  const transcript = [...agent.transcript];
  for (let i = transcript.length - 1; i >= 0; i--) {
    const entry = transcript[i];
    if (entry.kind !== "tool") continue;
    const callIdx = entry.calls.findIndex(
      (c) => c.id === event.tool_call_id && c.result === null,
    );
    if (callIdx === -1) continue;
    const calls = [...entry.calls];
    calls[callIdx] = {
      ...calls[callIdx],
      liveOutput: appendLiveOutputTail(calls[callIdx].liveOutput, event.text),
    };
    transcript[i] = { ...entry, calls };
    return { agent: { ...agent, transcript } };
  }
  return { agent };
};

/**
 * tool_result: update the matching call's result in place (which also clears
 * that call's `liveOutput` live tail), clear the pending approval, and (for
 * file_edit/file_write) capture a diff snapshot so the DiffViewer keeps
 * showing the diff after the approval resolves.
 */
export const reduceToolResult: Reducer<Ev<"tool_result">> = (agent, event) => {
  const next = { ...agent };
  flushStreamingText(next);
  const transcript = [...next.transcript];
  // Track the completed call so we can capture a diff snapshot below.
  let completedCall: { args: string } | null = null;
  let completedToolName: string | null = null;
  // Memory tools render as a readable `memory` entry (not a ToolCard). Find
  // the matching in-flight memory entry (the last one with the same name
  // still running) and finalize it with the tier/title/snippet parsed from
  // the result.
  let memoryFinalized = false;
  for (let i = transcript.length - 1; i >= 0; i--) {
    const entry = transcript[i];
    // Correlate by the owning call's id: a result finalizes exactly the
    // memory entry its tool_call_id belongs to. Without this gate ANY
    // tool's result (e.g. a parallel git_read) finalized the first running
    // memory card it found AND skipped the tool loop below — the foreign
    // card never received its result and stayed "running" forever
    // (regression pin in useAgentStore.test.ts).
    if (
      entry.kind === "memory" &&
      entry.running &&
      MEMORY_TOOLS.has(entry.name) &&
      entry.id === event.tool_call_id
    ) {
      const { tier, title, snippet, matched, hits } = parseMemoryEntry(entry.name, event.result);
      transcript[i] = {
        ...entry,
        tier,
        title,
        snippet,
        matched,
        // memory_search's parsed hit list drives the card's expandable row
        // (absent for the other memory tools).
        hits: hits?.length ? hits : undefined,
        success: event.result.success,
        running: false,
      };
      memoryFinalized = true;
      completedToolName = entry.name;
      break;
    }
  }
  if (!memoryFinalized) {
    for (let i = transcript.length - 1; i >= 0; i--) {
      const entry = transcript[i];
      if (entry.kind !== "tool") continue;
      const callIdx = entry.calls.findIndex((c) => c.id === event.tool_call_id);
      if (callIdx !== -1) {
        const calls = [...entry.calls];
        // The result is the complete truth: drop the live tail so the card
        // stops rendering the streaming preview.
        calls[callIdx] = {
          ...calls[callIdx],
          result: event.result,
          liveOutput: undefined,
          // Timing stamp (tool-card duration display): when this call's
          // result landed — with the call's startedAt it yields the duration.
          endedAt: Date.now(),
        };
        transcript[i] = { ...entry, calls };
        completedCall = calls[callIdx];
        completedToolName = entry.name;
        break;
      }
    }
  }
  next.transcript = transcript;
  // Error-streak tracking for the doom sound: a failed tool result extends
  // the streak, a successful one resets it (a long turn with occasional
  // failures must not accumulate to the cap unfairly). User denials and
  // interrupts are EXCLUDED — deliberate safety choices, not a stuck model —
  // and neither extend nor reset the streak, mirroring the backend
  // MAX_RETRIES counter exactly (review finding 2).
  const isDenial =
    !event.result.success &&
    typeof event.result.output === "string" &&
    isUserDenialToolOutput(event.result.output);
  next.consecutiveToolErrors = event.result.success
    ? 0
    : isDenial
      ? agent.consecutiveToolErrors
      : agent.consecutiveToolErrors + 1;
  // Snapshot preview from the matching pending approval *before* clearing it
  // so lastDiff can keep the Rust-side unified diff after the call finishes.
  const resolvedPreview =
    next.pendingApproval && next.pendingApproval.toolCallId === event.tool_call_id
      ? next.pendingApproval.preview
      : null;
  if (next.pendingApproval && next.pendingApproval.toolCallId === event.tool_call_id) {
    next.pendingApproval = null;
  }
  // The completed call's tool name (defaults to "tool" when the name is
  // unknown — e.g. a memory tool suppressed from the transcript).
  const toolName = completedToolName ?? "tool";
  // When a file_edit/file_write/file_append completes, capture a diff snapshot
  // so the DiffViewer keeps showing the diff after the approval resolves
  // (instead of snapping back to the empty state). Don't switch the active tab
  // — the user stays on whatever they were viewing. The same snapshot also
  // feeds the current top plan's changed-file list (planDiff effect).
  // multi_edit is deliberately absent: a snapshot entry is ONE path, while a
  // multi-file edit is N — its combined diff renders inline on the tool card
  // (Message.tsx `editDiff`, toolCardPaths.fileEditDiff) instead.
  let lastDiff: LastDiff | null | undefined;
  if (
    completedCall &&
    (toolName === "file_edit" ||
      toolName === "file_write" ||
      toolName === "file_append")
  ) {
    let parsedArgs: Record<string, unknown> = {};
    try {
      parsedArgs = JSON.parse(completedCall.args || "{}");
    } catch {
      // leave empty
    }
    const previewPath =
      resolvedPreview !== null &&
      resolvedPreview !== undefined &&
      resolvedPreview.kind !== "multi_diff" &&
      typeof resolvedPreview.path === "string"
        ? resolvedPreview.path
        : undefined;
    const unifiedDiff =
      resolvedPreview?.kind === "diff" && typeof resolvedPreview.diff === "string"
        ? resolvedPreview.diff
        : undefined;
    const previewContent =
      resolvedPreview?.kind === "new_file" &&
      typeof resolvedPreview.content === "string"
        ? resolvedPreview.content
        : undefined;
    lastDiff = {
      toolName,
      path: previewPath ?? (parsedArgs.path as string) ?? "",
      oldString: parsedArgs.old_string as string | undefined,
      newString: parsedArgs.new_string as string | undefined,
      content:
        previewContent ?? (parsedArgs.content as string | undefined),
      unifiedDiff,
      success: event.result.success,
      timestamp: Date.now(),
    };
  }
  return {
    agent: next,
    effects: {
      ...(lastDiff != null ? { lastDiff, planDiff: lastDiff } : {}),
    },
  };
};

/**
 * usage: accumulate token usage across the turn (a usage event arrives once
 * per LLM request; a multi-step turn makes several) and capture per-request
 * timing + tok/sec for the InflightBar. Also accumulates the session-level
 * timing (persists across turns) — the totals feed the Stats view's aggregate
 * rates, and the rolling last-3 window (`recentTiming`) feeds the status
 * bar's tok/sec — and records the last request's context breakdown for the
 * ctx hover popup.
 */
export const reduceUsage: Reducer<Ev<"usage">> = (agent, event) => {
  const ttft = event.ttft_ms ?? null;
  const gen = event.generation_ms ?? null;
  const inputTokSec =
    ttft && ttft > 0 ? event.prompt_tokens / (ttft / 1000) : null;
  const outputTokSec =
    gen && gen > 0 ? event.completion_tokens / (gen / 1000) : null;

  // Session-level accumulation (persists across turns — not reset on Started).
  // Only count a timed request when at least one of ttft/gen is present.
  const timed = ttft !== null || gen !== null;
  // Rolling window for the status bar's tok/sec: only requests that reported
  // a generation time enter it (a sample with no time denominator would
  // inflate the rate with tokens that have no time). Oldest dropped once the
  // window is full — the display tracks the model's CURRENT speed, not the
  // session-wide average.
  const recentTiming =
    gen !== null && gen > 0
      ? [
          ...agent.sessionTiming.recentTiming,
          { completion_tokens: event.completion_tokens, generation_ms: gen },
        ].slice(-RECENT_TIMING_WINDOW)
      : agent.sessionTiming.recentTiming;
  const sessionTiming = {
    ttft_ms_total: agent.sessionTiming.ttft_ms_total + (ttft ?? 0),
    generation_ms_total: agent.sessionTiming.generation_ms_total + (gen ?? 0),
    timed_requests: agent.sessionTiming.timed_requests + (timed ? 1 : 0),
    prompt_tokens: agent.sessionTiming.prompt_tokens + event.prompt_tokens,
    completion_tokens:
      agent.sessionTiming.completion_tokens + event.completion_tokens,
    recentTiming,
  };

  return {
    agent: {
      ...agent,
      tokenUsage: {
        prompt: agent.tokenUsage.prompt + event.prompt_tokens,
        completion: agent.tokenUsage.completion + event.completion_tokens,
        reasoning: agent.tokenUsage.reasoning + event.reasoning_tokens,
        cached: agent.tokenUsage.cached + event.cached_tokens,
      },
      lastRequestTiming: {
        ttft_ms: ttft,
        generation_ms: gen,
        input_tok_sec: inputTokSec,
        output_tok_sec: outputTokSec,
      },
      sessionTiming,
      lastContextBreakdown: {
        prompt: event.prompt_tokens,
        completion: event.completion_tokens,
        reasoning: event.reasoning_tokens,
        cached: event.cached_tokens,
      },
      // The authoritative token count for this request arrived — drop the
      // chars/4 streaming estimates (they were already added to the display
      // targets; the accumulated buckets now carry the real numbers).
      liveCompletionTokens: 0,
      liveReasoningTokens: 0,
    },
  };
};

/** context_usage: record the latest context-window fill + per-role breakdown. */
export const reduceContextUsage: Reducer<Ev<"context_usage">> = (agent, event) => {
  // Lever 6 (backlog e4a50d22): the S–F grade rides this event, but only on the
  // top-of-loop and post-compaction emissions — the mid-stream provider-exact
  // re-anchor omits it. An ABSENT quality therefore means "unchanged": keep the
  // last known grade so the popup badge never blinks off mid-turn.
  const quality = event.quality ?? agent.contextUsage.quality;
  return {
    agent: {
      ...agent,
      contextUsage: quality
        ? { used: event.used, max: event.max, quality }
        : { used: event.used, max: event.max },
      contextBreakdown: event.breakdown,
    },
  };
};

/**
 * compacted: context compaction finished — append a transcript confirmation
 * (`Context compacted: 500K → 24K`), or a "nothing to compact" note when the
 * summarization completed without a reduction (already summarized / too few
 * messages) so every `compact_started` announcement has a paired end. Pure
 * bookkeeping: `running` is unchanged (the backend emits this between turns /
 * mid-turn on the auto path, and the agent stays alive either way).
 */
export const reduceCompacted: Reducer<Ev<"compacted">> = (agent, event) => {
  const next = { ...agent };
  pushTranscriptEntry(next, {
    kind: "assistant",
    text:
      event.after < event.before
        ? `Context compacted: ${fmtTokens(event.before)} → ${fmtTokens(event.after)}`
        : "Nothing to compact — already compact.",
  });
  return { agent: next };
};

/**
 * compact_started: context compaction began — append the transcript
 * announcement (`Compacting context…`) so every path (slash command, popup
 * button, mid-turn, auto) announces its start; paired with `compacted`
 * (completion) or `error` (failure). Pure bookkeeping: `running` unchanged.
 */
export const reduceCompactStarted: Reducer<Ev<"compact_started">> = (agent) => {
  const next = { ...agent };
  pushTranscriptEntry(next, {
    kind: "assistant",
    text: "Compacting context…",
  });
  return { agent: next };
};

/**
 * vision_describe: a vision-model image description is starting — append a
 * `vision` transcript entry (running) so the transcript shows an "image
 * parsing" card instead of pausing silently (the fallback runs BEFORE the
 * turn starts, so `started` alone would leave the UI dark). A re-announced
 * index replaces its still-running entry (provider retry) instead of
 * stacking a duplicate. Paired with `vision_described`.
 */
export const reduceVisionDescribe: Reducer<Ev<"vision_describe">> = (
  agent,
  event,
) => {
  const next = { ...agent };
  const transcript = [...next.transcript];
  const entry = {
    kind: "vision" as const,
    index: event.index,
    total: event.total,
    query: event.query,
    description: null,
    success: true,
    running: true,
  };
  const runningIdx = transcript.findIndex(
    (e) => e.kind === "vision" && e.running && e.index === event.index,
  );
  if (runningIdx >= 0) {
    // Provider retry: replace the still-running entry but SPREAD the
    // previous one so its identity fields (entryId, ts) carry over — a
    // fresh literal would get a new entryId from the identity pass and
    // the card's React key would change mid-stream (review LOW 1,
    // plan 225e0dad).
    transcript[runningIdx] = { ...transcript[runningIdx], ...entry };
  } else {
    transcript.push(entry);
  }
  next.transcript = capTranscript(transcript);
  return { agent: next };
};

/**
 * vision_described: the paired vision call resolved — fill the matching
 * `vision` entry (by 1-based index; fall back to the last still-running
 * vision entry — events arrive in order, so an unmatched index can only
 * mean the store reloaded mid-loop) with the description/error and mark it
 * done. An entry with no match at all is left alone (the card was cleared
 * with the transcript; nothing to show).
 */
export const reduceVisionDescribed: Reducer<Ev<"vision_described">> = (
  agent,
  event,
) => {
  const next = { ...agent };
  const transcript = [...next.transcript];
  let idx = transcript.findIndex(
    (e) => e.kind === "vision" && e.running && e.index === event.index,
  );
  if (idx < 0) {
    for (let i = transcript.length - 1; i >= 0; i--) {
      const e = transcript[i];
      if (e.kind === "vision" && e.running) {
        idx = i;
        break;
      }
    }
  }
  if (idx < 0) {
    return { agent };
  }
  const entry = transcript[idx];
  if (entry.kind !== "vision") {
    return { agent };
  }
  transcript[idx] = {
    ...entry,
    description: event.description,
    success: event.success,
    running: false,
  };
  next.transcript = capTranscript(transcript);
  return { agent: next };
};

/**
 * approval_request: flush pre-approval text (it belongs above the approval
 * prompt) and record the pending approval.
 */
export const reduceApprovalRequest: Reducer<Ev<"approval_request">> = (
  agent,
  event,
) => {
  const next = { ...agent };
  flushStreamingText(next);
  next.pendingApproval = {
    toolCallId: event.tool_call_id,
    toolName: event.tool_name,
    args: event.args,
    preview: event.preview ?? null,
    coreOperation: event.core_operation ?? false,
  };
  // Needs-input ping: the agent is blocked on the user (gated by the
  // store's soundInput flag at execution time).
  return { agent: next, effects: { sound: "input" } };
};

/**
 * user_question: flush pre-question text (it belongs above the question
 * prompt) and record the pending question. Mirrors reduceApprovalRequest.
 */
export const reduceUserQuestion: Reducer<Ev<"user_question">> = (
  agent,
  event,
) => {
  const next = { ...agent };
  flushStreamingText(next);
  next.pendingQuestion = {
    questionId: event.question_id,
    question: event.question,
    options: event.options ?? [],
  };
  // Needs-input ping: same trigger as an approval request — the agent is
  // blocked on the user (gated by the store's soundInput flag).
  return { agent: next, effects: { sound: "input" } };
};

/**
 * Record that the user answered a pending `ask_user` question. Appends a `qa`
 * transcript entry (question + answer text) so the Q→A persists in the
 * conversation right where the live prompt was, sets `pendingQuestionAnswered`
 * so the live `QuestionPrompt` collapses into a static view immediately, and
 * clears `pendingQuestion` — the ask_user pause is TERMINAL for the display
 * state: the backend resumes the SAME turn mid-loop (it never emits a fresh
 * `started` — that event fires once per turn), so without clearing here the
 * InflightBar would sit on "waiting for your answer…" and the question prompt
 * would linger for the rest of the turn. Also clears `freeformQuestionId` so
 * any in-flight freeform-answer mode for this question is torn down (the
 * question is answered — no more typing needed).
 *
 * `answer` is the display text: the chosen option's label (for a choice) or the
 * typed text (for freeform).
 */
export const reduceQuestionAnswered = (
  agent: AgentState,
  payload: { questionId: string; question: string; answer: string },
): ReducerResult => {
  const next = { ...agent };
  pushTranscriptEntry(next, {
    kind: "qa",
    question: payload.question,
    answer: payload.answer,
    // Hover timestamp (plan afa81f0a) + stable entryId (React keys,
    // mem-perf review HIGH 3): this reducer is also invoked directly via
    // recordQuestionAnswer, bypassing applyAgentEvent's identity-based
    // stamping — stamp here so qa entries get both on every path.
    ts: Date.now(),
    entryId: allocEntryId(),
  });
  next.pendingQuestionAnswered = payload.questionId;
  // The question is answered — the backend is already resuming the same turn
  // with the answer. Terminate the pending-question display state NOW (the
  // `started` event that used to do this only fires at turn start, which for
  // a mid-turn ask_user never comes again).
  next.pendingQuestion = null;
  // Tear down any in-flight freeform-answer mode for this question — the
  // question is now answered, so the InputBar must not stay in answer mode.
  next.freeformQuestionId = null;
  return { agent: next };
};

/**
 * workflow_state_changed: record the phase for THIS agent (each agent owns
 * its workflow) and bump planVersion so PlanProgress re-fetches immediately.
 * A `complete` state also requests the completion ding.
 *
 * NOTE on repeat dings (review finding 3, 2026-08-20): the backend emits
 * this event after every SUCCESSFUL workflow tool call with the post-call
 * state — it is NOT transition-gated — so a `complete` state CAN repeat,
 * e.g. a skill that starts and ends in Complete (merge_to_main) re-enters
 * complete at skill_end and dings again. That re-ding is accepted: the
 * agent literally hit the complete state again. Resting-Complete across
 * turns never re-dings (no event is emitted while the agent is idle).
 */
export const reduceWorkflowStateChanged =
  (agentId: AgentId): Reducer<Ev<"workflow_state_changed">> =>
  (agent, event) => ({
    agent,
    effects: {
      workflowStates: { [agentId]: event.state as WorkflowState },
      planVersionBump: true,
      ...(event.state === "complete" ? { sound: "complete" as SoundKind } : {}),
    },
  });

/** step_completed: bump planVersion so PlanProgress re-fetches immediately. */
export const reduceStepCompleted: Reducer<Ev<"step_completed">> = (agent) => ({
  agent,
  effects: { planVersionBump: true },
});

/**
 * suggestion_injected: mark the oldest pending steer as landed, schedule its
 * auto-removal (dispatcher owns the setTimeout), and push a `steer`
 * transcript entry so it's visible with a highlight.
 */
export const reduceSuggestionInjected =
  (agentId: AgentId): Reducer<Ev<"suggestion_injected">> =>
  (agent, event) => {
    const next = { ...agent };
    const steers = [...next.steers];
    const pendingIdx = steers.findIndex((e) => e.status === "pending");
    let scheduleSteerRemoval: Effects["scheduleSteerRemoval"];
    if (pendingIdx !== -1) {
      steers[pendingIdx] = {
        ...steers[pendingIdx],
        status: "landed",
        timestamp: Date.now(),
      };
      scheduleSteerRemoval = { agentId, steerId: steers[pendingIdx].id };
    }
    next.steers = steers;
    // The steer's images ride the event — render them in the transcript
    // entry like a user prompt's (mirroring reducePromptDispatched).
    const entry: TranscriptEntry = {
      kind: "steer",
      text: event.text,
      ...(event.images.length > 0 ? { images: event.images } : {}),
    };
    pushTranscriptEntry(next, entry);
    return {
      agent: next,
      effects: scheduleSteerRemoval ? { scheduleSteerRemoval } : undefined,
    };
  };

/**
 * prompt_dispatched: a prompt was dispatched to this agent from outside the
 * main input (backlog dispatch — manual ▶, auto-feed, or Run-All). Append it
 * as a user transcript entry so it shows prominently as the goal at the top
 * of the conversation — mirroring how `handleSend` optimistically appends the
 * user message before the agent starts streaming. Any pending streaming text
 * is flushed first so the prompt lands above in-flight assistant text.
 */
export const reducePromptDispatched: Reducer<Ev<"prompt_dispatched">> = (agent, event) => {
  const next = { ...agent };
  const entry: TranscriptEntry = {
    kind: "user",
    text: event.text,
    ...(event.images.length > 0 ? { images: event.images } : {}),
  };
  pushTranscriptEntry(next, entry);
  return { agent: next };
};

/**
 * skill_started: a skill was started on this agent from the toolbar path
 * (`enter_skill` / the "Merge to main" button). Push a `skill` transcript
 * entry so the skill announces itself in the transcript like a tool call does
 * (`▶ skill "name" start`), mirroring how `skill_end` already appears. The
 * agent-driven `skill_start` tool announces itself via its own ToolCard, so
 * it does not emit this event. Any pending streaming text is flushed first so
 * the announcement lands above in-flight assistant text.
 */
export const reduceSkillStarted: Reducer<Ev<"skill_started">> = (agent, event) => {
  const next = { ...agent };
  pushTranscriptEntry(next, { kind: "skill", name: event.name, prompt: event.prompt });
  return { agent: next };
};

/** Merge an `agentProviders` patch: non-empty names upsert, `null`/empty
 *  clears (the agent falls back to model-id resolution). Kept tiny + pure so
 *  the clear-on-null semantics are unit-testable without the store. */
export function mergeAgentProviders(
  base: Record<AgentId, string>,
  patch: Record<AgentId, string | null>,
): Record<AgentId, string> {
  const next = { ...base };
  for (const [id, name] of Object.entries(patch)) {
    // Object.entries keys are strings (numeric keys stringify); convert back
    // to the numeric AgentId before use.
    const key = Number(id) as AgentId;
    if (name === null || name === "") {
      delete next[key];
    } else {
      next[key] = name;
    }
  }
  return next;
}

/**
 * model_changed: the active model for this agent changed (the IPC layer
 * swapped the provider via `set_model` / `save_endpoints`, or a per-turn
 * override resolved differently). Stamp the new model id into `agentModels`
 * AND the serving endpoint into `agentProviders` so the StatusBar label +
 * the agent tab's second line reflect it immediately — no `list_agents`
 * poll needed. The provider names the endpoint that actually serves the
 * model (backlog 2980ca67: a model id listed under two endpoints must not
 * be labeled by first-match); an absent/empty name CLEARS the agent's
 * entry so the UI falls back to resolving the model id. The reducer returns
 * the agent unchanged (it's a display-only event); the effects carry both
 * stamps for the dispatcher to merge.
 */
export const reduceModelChanged =
  (agentId: AgentId): Reducer<Ev<"model_changed">> =>
  (agent, event) => ({
    agent,
    effects: {
      agentModels: { [agentId]: event.model },
      agentProviders: {
        [agentId]: event.provider && event.provider !== "" ? event.provider : null,
      },
      // The wire-reported effective effort of the model now serving (backlog
      // 51dab4da): present upserts, absent CLEARS — the status bar must
      // never keep the previous model's effort against the new one.
      agentWireEfforts: {
        [agentId]:
          event.reasoning_effort && event.reasoning_effort !== ""
            ? event.reasoning_effort
            : null,
      },
    },
  });

/**
 * memory_recalled: the per-turn auto-recall injected memories into the
 * agent's prompt. Rendered as a read-only `memory` transcript entry so
 * memory/self-learning usage is visible in the agent window (user request
 * 2026-04-20). Not a tool call — the entry is pushed already finalized
 * (running: false), so reduceToolResult never touches it (name "auto-recall"
 * is not in MEMORY_TOOLS). Visibility is a render-time concern: the entry
 * always enters the store; Conversation.tsx hides it when show_tool_activity
 * is off.
 */
export const reduceMemoryRecalled: Reducer<Ev<"memory_recalled">> = (agent, event) => {
  const next = { ...agent };
  const transcript = [...next.transcript];
  transcript.push({
    kind: "memory",
    name: "auto-recall",
    tier: "recall",
    title: undefined,
    snippet: `${event.hits.length} hit(s) — top: ${event.hits[0]?.title ?? "—"}`,
    hits: event.hits,
    success: true,
    running: false,
  });
  next.transcript = capTranscript(transcript);
  return { agent: next };
};

/**
 * Finalize every still-running transcript card with a failed placeholder.
 *
 * Terminal events (`finished`, a final `error`, `child_finished`) are the
 * last signal the UI gets for the turn: a card still running at that point
 * can never complete — a steer/interrupt cut the stream after
 * `tool_call_start` but before the call got a result (backlog 63cbc20f),
 * or the task died mid-call. Every announcement gets an end note (the
 * reduceError vision precedent). The placeholder is a guess, not a
 * verdict: a late REAL `tool_result` still overwrites it because
 * `reduceToolResult` matches by tool_call_id. Retry-path errors keep cards
 * running (the call may still be in flight) and must NOT call this.
 */
export function sweepRunningCards(
  transcript: TranscriptEntry[],
  note: string,
): TranscriptEntry[] {
  return transcript.map((entry) => {
    if (entry.kind === "tool") {
      if (!entry.calls.some((c) => c.result === null)) return entry;
      return {
        ...entry,
        calls: entry.calls.map((c) =>
          c.result === null ? { ...c, result: { success: false, output: note } } : c,
        ),
      };
    }
    if (entry.kind === "memory") {
      return entry.running ? { ...entry, running: false, success: false } : entry;
    }
    if (entry.kind === "vision") {
      return entry.running
        ? { ...entry, running: false, success: false, description: note }
        : entry;
    }
    return entry;
  });
}

/** finished: stop the agent; move any pending streaming text into the transcript. */
export const reduceFinished: Reducer<Ev<"finished">> = (agent) => {
  const next = {
    ...agent,
    running: false,
    // A clean finish is not a failure — clear the Continue affordance.
    failed: false,
    phase: "idle" as const,
    turnStartedAt: null,
    liveCompletionTokens: 0,
    liveReasoningTokens: 0,
  };
  flushStreamingText(next);
  // Backlog 63cbc20f: a mid-stream steer/interrupt can cut the turn after a
  // tool_call_start but before its result — finalize any still-running card
  // so nothing spins forever once the turn is over.
  next.transcript = sweepRunningCards(next.transcript, "(interrupted)");
  // Bump planVersion so StatusBar's refreshPlan() re-fetches the true
  // backend workflow state when the agent goes idle. This corrects any stale
  // workflowStates value left by a workflow mutation that did NOT emit a
  // WorkflowStateChanged event — e.g. the harness/outer-agent calling
  // complete_step/create_plan/update_plan/abandon_plan through its own
  // dispatch path (outside the inner agent loop's run_turn, which is the
  // only place WorkflowStateChanged is emitted). Without this, the UI can
  // stay stuck at "executing" after the last step completes, hiding the
  // "Merge to main" button (gated on complete/planning) until a restart.
  return { agent: next, effects: { planVersionBump: true } };
};

/**
 * exited: the agent task terminated (inbox closed or cancelled) — drop it
 * from every map. The dispatcher removes it and falls back to the main agent
 * when the removed agent was active.
 */
export const reduceExited: Reducer<Ev<"exited">> = (agent) => ({
  agent,
  effects: { removeAgent: true },
});

/**
 * parked: the agent went idle awaiting input instead of auto-continuing —
 * store the pre-stall evidence. The InputBar surfaces the two manual-input
 * reasons (`interrupted`, `budget_exhausted`) as a distinct banner so an
 * interrupted turn never looks like a hang; the by-design reasons
 * (`waiting_for_descendants`, `no_work_expected`) are evidence-only.
 * Cleared by the next `started`.
 */
export const reduceParked: Reducer<Ev<"parked">> = (agent, event) => ({
  agent: {
    ...agent,
    parked: {
      reason: event.reason,
      workflow_state: event.workflow_state,
      descendants_running: event.descendants_running,
      auto_continue_streak: event.auto_continue_streak,
    },
  },
});

/**
 * error: preserve partial text, surface the error in the transcript +
 * activity log, and stop the agent on a final (non-retrying) error.
 */
export const reduceError: Reducer<Ev<"error">> = (agent, event) => {
  const next = { ...agent };
  flushStreamingText(next);
  // Backlog 63cbc20f (extending review LOW 1, 2026-08-27): on a FINAL error
  // sweep EVERY still-running card (tool, memory, vision), not just vision —
  // a task that dies mid-turn must not leave any card spinning forever.
  // Mirrors the CompactStarted pairing precedent: every announcement gets an
  // end note. A retrying error means the turn (and the in-flight calls)
  // continues, so cards stay running.
  const swept = event.retrying
    ? next.transcript
    : sweepRunningCards(next.transcript, "(interrupted)");
  next.transcript = capTranscript([
    ...swept,
    { kind: "error", text: event.error },
  ]);
  next.activityLog = capActivityLog([
    ...next.activityLog,
    { kind: "error" as const, text: event.error, timestamp: Date.now() },
  ]);
  // Every error event (retrying or final) extends the doom streak — the
  // provider-retry path surfaces its retries as retrying:true events, and
  // the user sees those the same way as failed tool calls.
  const streak = agent.consecutiveToolErrors + 1;
  next.consecutiveToolErrors = streak;
  // A retry note (retrying: true) is transient — keep running.
  // A final error (retrying: false) stops the agent.
  if (!event.retrying) {
    next.running = false;
    // The turn ended in a FINAL error — mark the agent as failed so the
    // InputBar shows the Continue button (`!running && failed`) and the user
    // can resume the dead task with a continuation prompt.
    next.failed = true;
    next.phase = "idle";
    next.turnStartedAt = null;
    next.liveCompletionTokens = 0;
    next.liveReasoningTokens = 0;
  }
  // On a final (non-retrying) error the agent goes idle, same as `finished`.
  // Bump planVersion so StatusBar re-fetches the true backend workflow state,
  // correcting any stale workflowStates value left by a workflow mutation that
  // didn't emit WorkflowStateChanged (mirrors reduceFinished; see its comment).
  return {
    agent: next,
    effects: {
      ...(!event.retrying ? { planVersionBump: true } : {}),
      // "Three errors and the agent just stops": the doom sound fires only
      // on the FINAL error with the streak (including this error) at
      // DOOM_ERROR_STREAK or more — a single fatal provider error stays
      // silent, and a streak broken by any success never reaches the cap.
      ...(!event.retrying && streak >= DOOM_ERROR_STREAK
        ? { sound: "doom" as SoundKind }
        : {}),
    },
  };
};

/**
 * child_finished: a tool-spawned background agent finished its task — mark it
 * not running, surface a neutral "done" note in its transcript, and record
 * its display name (so the top bar shows the real name even if the
 * listAgents refresh hasn't landed yet).
 */
export const reduceChildFinished: Reducer<Ev<"child_finished">> = (agent, event) => {
  // Reset the live turn-phase fields alongside `running` (symmetry with
  // reduceFinished / the final-error arm): the synthesized child_finished
  // normally follows the child's own terminal event which already reset
  // them, but a future reordering must not leave a stale phase visible
  // (review L3).
  const next = {
    ...agent,
    running: false,
    phase: "idle" as const,
    turnStartedAt: null,
    liveCompletionTokens: 0,
    liveReasoningTokens: 0,
  };
  flushStreamingText(next);
  // Backlog 63cbc20f: this terminal event ends the (child) agent — any
  // still-running card in ITS transcript can never complete, so end it
  // (mirrors reduceFinished).
  next.transcript = sweepRunningCards(next.transcript, "(interrupted)");
  next.transcript = capTranscript([
    ...next.transcript,
    {
      kind: "assistant" as const,
      text: event.success
        ? "✓ Background task finished."
        : "✗ Background task failed.",
    },
  ]);
  return {
    agent: next,
    effects: { agentNames: { [event.child_id]: event.name } },
  };
};

/**
 * Dispatch a single agent event: look up the per-kind reducer, compute the
 * new per-agent state, then apply the (optional) cross-map effects through
 * one uniform code path. Returns the new AppState for the zustand `set`.
 *
 * Pure with respect to the store (no `setTimeout` — the caller owns the
 * `scheduleSteerRemoval` side effect), so the whole path is unit-testable.
 */
export function applyAgentEvent(
  s: AppStateLike,
  agentId: AgentId,
  event: SerializableAgentEvent,
): { next: AppStateLike; effects?: Effects } {
  const agent = getOrCreate(s.agents, agentId);
  let result: ReducerResult;
  switch (event.kind) {
    case "started": result = reduceStarted(agent, event); break;
    case "phase": result = reducePhase(agent, event); break;
    case "text_delta": result = reduceTextDelta(agent, event); break;
    case "reasoning_delta": result = reduceReasoningDelta(agent, event); break;
    case "tool_call_start": result = reduceToolCallStart(agent, event); break;
    case "tool_call_arg_delta": result = reduceToolCallArgDelta(agent, event); break;
    case "tool_output_delta": result = reduceToolOutputDelta(agent, event); break;
    case "tool_result": result = reduceToolResult(agent, event); break;
    case "usage": result = reduceUsage(agent, event); break;
    case "context_usage": result = reduceContextUsage(agent, event); break;
    case "compacted": result = reduceCompacted(agent, event); break;
    case "compact_started": result = reduceCompactStarted(agent, event); break;
    case "vision_describe": result = reduceVisionDescribe(agent, event); break;
    case "vision_described": result = reduceVisionDescribed(agent, event); break;
    case "approval_request": result = reduceApprovalRequest(agent, event); break;
    case "user_question": result = reduceUserQuestion(agent, event); break;
    case "workflow_state_changed": result = reduceWorkflowStateChanged(agentId)(agent, event); break;
    case "step_completed": result = reduceStepCompleted(agent, event); break;
    case "suggestion_injected": result = reduceSuggestionInjected(agentId)(agent, event); break;
    case "prompt_dispatched": result = reducePromptDispatched(agent, event); break;
    case "skill_started": result = reduceSkillStarted(agent, event); break;
    case "model_changed": result = reduceModelChanged(agentId)(agent, event); break;
    case "memory_recalled": result = reduceMemoryRecalled(agent, event); break;
    case "finished": result = reduceFinished(agent, event); break;
    case "parked": result = reduceParked(agent, event); break;
    case "exited": result = reduceExited(agent, event); break;
    case "error": result = reduceError(agent, event); break;
    case "child_finished": result = reduceChildFinished(agent, event); break;
    default: result = { agent: agent }; break;
  }

  const effects = result.effects;

  // Stamp newly-created transcript entries with their arrival time (hover
  // timestamps, plan afa81f0a) and a stable monotonic entryId (React keys,
  // mem-perf review HIGH 3). Identity-based: only entry objects that are
  // NOT in the pre-event transcript AND lack the field get stamped —
  // entries restored from a persisted conversation keep their saved values
  // (or stay unstamped for legacy saves), and mutation cases that spread
  // the previous entry keep them. The per-kind reducers are pure and
  // return fresh objects, so mutating the new entries here (pre-write) is
  // safe.
  if (result.agent.transcript !== agent.transcript) {
    const old = new Set(agent.transcript);
    const now = Date.now();
    for (const e of result.agent.transcript) {
      if (old.has(e)) continue;
      if (e.ts === undefined) e.ts = now;
      if (e.entryId === undefined) e.entryId = allocEntryId();
    }
  }

  // Per-top-plan changed-file list: upsert the effect's snapshot, and reset
  // (along with lastDiff + the dropdown selection) exactly when the ROOT plan
  // id changes for the MAIN agent — a fresh top-level plan started. Sub-plan
  // push/pop and skill transitions keep the same root id, so they never reset.
  const planDiffs = effects?.planDiff
    ? upsertPlanDiff(s.planDiffs, effects.planDiff)
    : s.planDiffs;
  let topPlanId = s.topPlanId;
  let finalPlanDiffs = planDiffs;
  let selectedDiffPath = s.selectedDiffPath;
  let lastDiff =
    effects && "lastDiff" in effects ? (effects.lastDiff ?? null) : s.lastDiff;
  if (event.kind === "workflow_state_changed") {
    // Track the root plan id from any agent's event (agents share the root
    // plan in practice, so this converges to the same id).
    topPlanId = event.top_plan_id;
    // Reset the changed-file list exactly when the ROOT plan id changes for
    // the MAIN agent — a fresh top-level plan started. Sub-plan push/pop and
    // skill transitions keep the same root id, and a subagent's event can
    // never reset the main plan's list.
    if (
      agentId === selectMainAgentId(s.agentParents, s.agents) &&
      event.top_plan_id !== s.topPlanId
    ) {
      finalPlanDiffs = [];
      selectedDiffPath = null;
      lastDiff = null;
    }
  }

  // Cross-map removal (exited): drop the agent from every map, and fall back
  // to the main agent when the removed agent was active.
  if (effects?.removeAgent) {
    const agents = { ...s.agents };
    delete agents[agentId];
    const agentNames = { ...s.agentNames };
    delete agentNames[agentId];
    const agentParents = { ...s.agentParents };
    delete agentParents[agentId];
    const agentModels = { ...s.agentModels };
    delete agentModels[agentId];
    const agentProviders = { ...s.agentProviders };
    delete agentProviders[agentId];
    const agentEfforts = { ...s.agentEfforts };
    delete agentEfforts[agentId];
    const agentWireEfforts = { ...s.agentWireEfforts };
    delete agentWireEfforts[agentId];
    const workflowStates = { ...s.workflowStates };
    delete workflowStates[agentId];
    let activeAgent = s.activeAgent;
    if (activeAgent === agentId) {
      activeAgent = selectMainAgentId(agentParents, agents);
    }
    return { next: { ...s, agents, agentNames, agentParents, agentModels, agentProviders, agentEfforts, agentWireEfforts, workflowStates, activeAgent }, effects };
  }

  // The uniform single write path for every other event kind.
  return {
    next: {
      ...s,
      agents: { ...s.agents, [agentId]: result.agent },
      planDiffs: finalPlanDiffs,
      topPlanId,
      selectedDiffPath,
      lastDiff,
      ...(effects?.planVersionBump ? { planVersion: s.planVersion + 1 } : {}),
      ...(effects?.workflowStates
        ? { workflowStates: { ...s.workflowStates, ...effects.workflowStates } }
        : {}),
      ...(effects?.agentNames
        ? { agentNames: { ...s.agentNames, ...effects.agentNames } }
        : {}),
      ...(effects?.agentModels
        ? { agentModels: { ...s.agentModels, ...effects.agentModels } }
        : {}),
      ...(effects?.agentProviders
        ? {
            agentProviders: mergeAgentProviders(s.agentProviders, effects.agentProviders),
          }
        : {}),
      // Same upsert/clear-on-null semantics as agentProviders (backlog
      // 51dab4da): the wire effort clears when the backend doesn't know it.
      ...(effects?.agentWireEfforts
        ? {
            agentWireEfforts: mergeAgentProviders(
              s.agentWireEfforts,
              effects.agentWireEfforts,
            ),
          }
        : {}),
    },
    effects,
  };
}
