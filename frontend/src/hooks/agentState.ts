// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Pure agent-state types + helpers for the frontend store.
//
// Extracted from `useAgentStore.ts` (Maint H3): this module owns the
// per-agent state shape (`AgentState`), the supporting DTOs, and the pure
// helpers the per-event reducers rely on (`emptyAgentState`, `getOrCreate`,
// `selectMainAgentId`, `flushStreamingText`, `pushTranscriptEntry`,
// truncation). Nothing here touches the Zustand store or the DOM — it is
// fully unit-testable.

import type {
  AgentId,
  ApprovalPreview,
  ContextQuality,
  TranscriptEntry,
  TurnPhase,
} from "../lib/types";

/** A pending tool approval awaiting the user's decision. */
export interface PendingApproval {
  toolCallId: string;
  toolName: string;
  args: unknown;
  /**
   * Rust-side pure preview (unified diff / new-file body) when the tool
   * provided one. Prefer this over reconstructing from `args` in the UI.
   */
  preview: ApprovalPreview | null;
  /**
   * True for core operations (git merge/push) that always prompt regardless
   * of safety mode or rules. The UI hides the no-op "Mark Safe" / "Allow for
   * project" buttons when true (both are bypassed by never_auto_for).
   */
  coreOperation: boolean;
}

/** One clickable option in an `ask_user` question. */
export interface QuestionOption {
  /** The short button label. */
  label: string;
  /** An optional longer description shown under the label. */
  description?: string | null;
}

/** A pending `ask_user` question awaiting the user's answer. */
export interface PendingQuestion {
  /** A unique id for this question (matches the answer). */
  questionId: string;
  /** The question text. */
  question: string;
  /** The clickable options (may be empty — the freeform input is always shown). */
  options: QuestionOption[];
}

/** Token usage from a Usage event. */
export interface TokenUsage {
  prompt: number;
  completion: number;
  reasoning: number;
  /** Prompt tokens served from the provider's cache (subset of prompt). */
  cached: number;
}

/** Timing + tok/sec from the most recent Usage event, for the InflightBar. */
export interface RequestTiming {
  ttft_ms: number | null;
  generation_ms: number | null;
  /** Input tok/sec = prompt_tokens / ttft_s (null when ttft unavailable). */
  input_tok_sec: number | null;
  /** Output tok/sec = completion_tokens / generation_s (null when unavailable). */
  output_tok_sec: number | null;
}

/** How many recent requests the InflightBar status bar's tok/sec window
 * covers (oldest dropped as newer requests land). */
export const RECENT_TIMING_WINDOW = 3;

/**
 * Session-level timing accumulator. Persists across turns (reset only on
 * clearConversation, not on Started). The totals accumulate over ALL requests
 * and feed the Stats view's aggregate rates (total tokens / total time via
 * `aggregateTokPerSec` — NOT total tokens / avg-per-request time, which
 * inflates by the request count); `recentTiming` is the rolling last-3
 * window the InflightBar status bar's tok/sec reads from.
 */
export interface SessionTiming {
  /** Sum of TTFT (ms) across all timed requests in the session. */
  ttft_ms_total: number;
  /** Sum of generation time (ms) across all timed requests. */
  generation_ms_total: number;
  /** Number of requests that had timing (ttft or generation) reported. */
  timed_requests: number;
  /** Total prompt tokens across the session (for the weighted input rate). */
  prompt_tokens: number;
  /** Total completion tokens across the session (for the weighted output rate). */
  completion_tokens: number;
  /**
   * Rolling window of the last RECENT_TIMING_WINDOW timed requests (oldest
   * dropped), newest last — feeds the InflightBar status bar's output tok/sec
   * so it tracks the model's CURRENT speed instead of drifting toward the
   * session average. Only requests that reported a generation time enter the
   * window (a sample with no time denominator would inflate the rate). The
   * totals above keep accumulating over ALL requests — the Stats view still
   * uses them.
   */
  recentTiming: Array<{ completion_tokens: number; generation_ms: number }>;
}

/**
 * Aggregate tok/sec across multiple requests: total tokens / total time.
 *
 * This is the correct way to combine per-request throughput into a session
 * or per-model aggregate. It must NOT divide by the *average* per-request
 * time (`ms_total / timed_requests`) — that yields `tokens × timed_requests
 * × 1000 / ms_total`, inflating the rate by the request count (10 requests
 * of 100 tok / 1000ms each would show 1000 tok/s instead of 100).
 *
 * Returns `null` when `msTotal` is 0 (no timed requests yet) so callers can
 * render a placeholder.
 *
 * @param tokens  Total tokens across all requests (prompt or completion).
 * @param msTotal Total time across all requests (ttft_ms_total or
 *                generation_ms_total), in milliseconds.
 * @returns Tokens per second, or `null` when there's no timing data.
 */
export function aggregateTokPerSec(
  tokens: number,
  msTotal: number,
): number | null {
  return msTotal > 0 ? tokens / (msTotal / 1000) : null;
}

/**
 * Output tok/sec over the rolling recent-request window
 * (`sessionTiming.recentTiming`): total completion tokens / total generation
 * time across the window, via `aggregateTokPerSec` — total-over-total, NOT
 * the average of the per-request rates (which would inflate the result).
 *
 * This is what the InflightBar status bar displays: a window over the last
 * few requests tracks the model's CURRENT speed (e.g. right after an
 * endpoint switch or a slow patch) instead of drifting toward the
 * session-wide average.
 *
 * Returns `null` when the window is empty (no timed requests yet) so callers
 * can render a placeholder.
 */
export function recentOutputTokPerSec(
  sessionTiming: SessionTiming,
): number | null {
  const tokens = sessionTiming.recentTiming.reduce(
    (sum, s) => sum + s.completion_tokens,
    0,
  );
  const ms = sessionTiming.recentTiming.reduce(
    (sum, s) => sum + s.generation_ms,
    0,
  );
  return aggregateTokPerSec(tokens, ms);
}

/**
 * The context breakdown of the most recent request — what the context window
 * is made of (prompt / completion / reasoning / cached). Shown in the ctx
 * bar's hover popup. `null` before the first Usage event.
 */
export interface LastContextBreakdown {
  prompt: number;
  completion: number;
  reasoning: number;
  cached: number;
}

/**
 * A queued steer (mid-work guidance) shown in the backlog above the input
 * box. `pending` = sent but not yet injected; `landed` = injected into the
 * conversation (the agent saw it). Landed steers auto-remove after a few
 * seconds.
 */
export interface SteerEntry {
  id: number;
  text: string;
  /** Pasted image attachments (base64 data URLs) carried with the steer. */
  images?: string[];
  status: "pending" | "landed";
  timestamp: number;
}

/** A reasoning/answer/error/info line in the activity log (the thinking box).
 *  `reasoning` = the model's thinking; `answer` = the live completion text
 *  mirrored as it streams (user request 2026-08-22 — both channels visible). */
export interface ActivityEntry {
  kind: "reasoning" | "answer" | "error" | "info";
  text: string;
  timestamp: number;
}

/** The per-agent slice of frontend state, updated by agent events. */
/** Pre-stall evidence carried by the backend `parked` event — why the
 * agent went idle awaiting input. */
export interface ParkedInfo {
  reason:
    | "interrupted"
    | "budget_exhausted"
    | "waiting_for_descendants"
    | "no_work_expected";
  /** The workflow state at the park (evidence label, e.g. "Reviewing"). */
  workflow_state: string;
  /** Whether spawned descendants were still running. */
  descendants_running: boolean;
  /** The auto-continue streak at the park. */
  auto_continue_streak: number;
}

export interface AgentState {
  transcript: TranscriptEntry[];
  streamingText: string;
  streamingReasoning: string;
  running: boolean;
  /**
   * True when the agent's LAST turn ended in a FINAL (non-retrying) error —
   * the task is dead and the agent sits idle. Cleared by the next `started`
   * (a fresh run), a successful `finished`, and a conversation clear. Drives
   * the InputBar's Continue button (`!running && failed`): the agent's task
   * loop survived (it can be prompted again), so the user can resume the
   * failed task with a continuation prompt.
   */
  failed: boolean;
  /**
   * Set when the agent PARKED (went idle awaiting input) instead of
   * auto-continuing — the pre-stall evidence from the backend `parked`
   * event. `reason` distinguishes the two manual-input cases
   * (`interrupted` — the user pressed Stop; `budget_exhausted` — the
   * auto-continue budget ran out mid-work) from the by-design waits
   * (`waiting_for_descendants`, `no_work_expected`). The InputBar shows a
   * distinct banner for the manual-input reasons so an interrupted turn
   * never looks like a hang (backlog 5c33e945, 2027-01-07 live: a
   * mid-Executing stop looked like a hang and needed a manual "c").
   * Cleared on the next turn (`started`) and on a conversation clear.
   */
  parked: ParkedInfo | null;
  pendingApproval: PendingApproval | null;
  /** A pending `ask_user` question awaiting the user's answer (null when none). */
  pendingQuestion: PendingQuestion | null;
  /**
   * The questionId of the most recently answered question, set the instant the
   * user picks an option / submits freeform text. `recordQuestionAnswer` also
   * clears `pendingQuestion` itself (the backend emits no fresh `started`
   * mid-turn), so the live `QuestionPrompt` unmounts right away — this marker
   * is a belt-and-suspenders guard making it render `null` for the answered
   * id; the static Q→A view is the appended `qa` transcript entry. Cleared on
   * the next turn (`started`).
   */
  pendingQuestionAnswered: string | null;
  /**
   * When set, the InputBar is in "freeform-answer mode" for this questionId:
   * the user picked the "💬 Let's talk about it" numbered choice and is typing
   * their answer in the standard command box. Enter submits it as a freeform
   * answer; Esc cancels (keeps the question pending). Cleared on answer or on
   * the next turn (`started`).
   */
  freeformQuestionId: string | null;
  activityLog: ActivityEntry[];
  tokenUsage: TokenUsage;
  /**
   * The live request-loop phase (sending / compacting / waiting /
   * streaming / running_tools; `idle` when no turn is running). Drives the
   * inflight bar's status label instead of the static "thinking…".
   */
  phase: TurnPhase;
  /**
   * Epoch ms when the current turn started (null when idle). Drives the
   * inflight bar's elapsed timer. Set only when a Started transitions
   * idle→running — mid-turn provider-retry re-Starts keep the original time.
   */
  turnStartedAt: number | null;
  /**
   * Live tokens-received estimates, split by bucket: chars/4 of the text
   * deltas (liveCompletionTokens) and of the reasoning deltas
   * (liveReasoningTokens) streamed since the last Usage event (the
   * authoritative count). Added to the accumulated completion / reasoning
   * tokens in the inflight bar so BOTH counters climb in the right bucket
   * DURING the stream — reasoning first, where a long thinking phase used
   * to leave the display frozen — then snap to the real numbers on usage.
   */
  liveCompletionTokens: number;
  liveReasoningTokens: number;
  /** Timing + tok/sec from the most recent Usage event (for the InflightBar). */
  lastRequestTiming: RequestTiming | null;
  /** Session-level timing accumulator for the weighted-mean tok/sec. Persists
   * across turns (reset only on clearConversation). */
  sessionTiming: SessionTiming;
  /** The context breakdown of the most recent request (for the ctx hover
   * popup). `null` before the first Usage event. */
  lastContextBreakdown: LastContextBreakdown | null;
  /** Context-window fill. `quality` is lever 6's S–F grade (backlog e4a50d22),
   * present only while the backend's `[general.optimizer] quality_score` flag
   * is on.
   * The reducer carries the last known grade across events that omit it, so it
   * is absent only until the first graded event. */
  contextUsage: { used: number; max: number; quality?: ContextQuality };
  /** Per-role token breakdown (system/user/assistant/tool) from the most
   * recent ContextUsage event, for the ctx hover popup. */
  contextBreakdown: { system: number; user: number; assistant: number; tool: number };
  /** Backlog of queued steers (mid-work guidance), newest last. */
  steers: SteerEntry[];
  /**
   * Consecutive errors this turn: failed tool results (user denials
   * excluded) AND error events (retrying or final). Feeds the
   * doom-notification trigger — when a FINAL error stops the agent with
   * this streak at [`DOOM_ERROR_STREAK`](./agentEventReducer) or more, the
   * doom sound plays. Resets on any successful tool result and on a
   * fresh-turn `started` (an idle→running Started; the mid-turn re-Starts
   * of provider retries keep it). Mirrors the backend `agent::MAX_RETRIES`
   * consecutive-error cap semantics.
   */
  consecutiveToolErrors: number;
}

/** Which tab is active in the right panel. */
export type RightPanelTab =
  | "plan"
  | "diff"
  | "git"
  | "files"
  | "stats"
  | "trace"
  | "backlog"
  | "browser"
  | "graph"
  | "memory";

/** All right-panel tool tabs, in display order. Used for per-tab enable/disable. */
export const ALL_RIGHT_PANEL_TABS: RightPanelTab[] = [
  "plan",
  "diff",
  "git",
  "files",
  "stats",
  "trace",
  "backlog",
  "browser",
  "graph",
  "memory",
];

/**
 * The most recent completed file-edit/file-write, kept around so the
 * DiffViewer can keep showing the diff after the approval resolves (instead
 * of snapping back to the empty state the instant the tool result arrives).
 */
export interface LastDiff {
  toolName: string;
  path: string;
  oldString?: string;
  newString?: string;
  content?: string;
  /**
   * Prefer when present: unified-diff text from Rust `ApprovalPreview::Diff`
   * (avoids re-running client LCS after the edit lands).
   */
  unifiedDiff?: string;
  success: boolean;
  timestamp: number;
}

/** A fresh, empty per-agent state. */
export function emptyAgentState(): AgentState {
  return {
    transcript: [],
    streamingText: "",
    streamingReasoning: "",
    running: false,
    failed: false,
    parked: null,
    pendingApproval: null,
    pendingQuestion: null,
    pendingQuestionAnswered: null,
    freeformQuestionId: null,
    activityLog: [],
    tokenUsage: { prompt: 0, completion: 0, reasoning: 0, cached: 0 },
    phase: "idle",
    turnStartedAt: null,
    liveCompletionTokens: 0,
    liveReasoningTokens: 0,
    lastRequestTiming: null,
    sessionTiming: {
      ttft_ms_total: 0,
      generation_ms_total: 0,
      timed_requests: 0,
      prompt_tokens: 0,
      completion_tokens: 0,
      recentTiming: [],
    },
    lastContextBreakdown: null,
    contextUsage: { used: 0, max: 0 },
    contextBreakdown: { system: 0, user: 0, assistant: 0, tool: 0 },
    steers: [],
    consecutiveToolErrors: 0,
  };
}

/** Return the agent's state, creating an empty one on first touch. */
export function getOrCreate(
  agents: Record<AgentId, AgentState>,
  id: AgentId,
): AgentState {
  return agents[id] ?? emptyAgentState();
}

/**
 * The main agent's id: the smallest id among parentless agents (the main
 * agent is registered first, before any UI- or tool-spawned agent). Falls
 * back to the smallest known id while parent info is still loading.
 * Exported as a pure helper so reactive zustand selectors (which must read
 * the maps inside the selector body) and the imperative `mainAgentId()`
 * store action share one implementation.
 */
export function selectMainAgentId(
  agentParents: Record<AgentId, AgentId | null>,
  agents: Record<AgentId, unknown>,
): AgentId | null {
  const parentless = Object.entries(agentParents)
    .filter(([, parent]) => parent === null)
    .map(([id]) => Number(id));
  if (parentless.length > 0) return Math.min(...parentless);
  const known = Object.keys(agents).map(Number);
  return known.length > 0 ? Math.min(...known) : null;
}

/** The minimal store snapshot shape `didMainTurnEnd` reads. */
export interface MainRunningSnapshot {
  agentParents: Record<AgentId, AgentId | null>;
  agents: Record<AgentId, AgentState>;
}

/**
 * Whether the MAIN agent's turn just resolved (running true→false) between
 * two store snapshots — the F1b turn-end signal that re-reads the git branch
 * in App.tsx (the agent may have committed/checked out mid-run).
 *
 * Only the true→false edge fires. All other transitions — false→false (idle
 * chatter), false→true (turn start), true→true (still running) — return
 * false. A missing main agent reads as `running: false` (the `?? false`
 * fallback), so the main agent being removed (or its id shifting to a
 * different, non-running agent) between snapshots while it was running also
 * fires — harmless-and-correct: at worst one extra branch refresh.
 */
export function didMainTurnEnd(
  prev: MainRunningSnapshot,
  next: MainRunningSnapshot,
): boolean {
  const runningOf = (s: MainRunningSnapshot): boolean => {
    const mainId = selectMainAgentId(s.agentParents, s.agents);
    return mainId !== null ? s.agents[mainId]?.running ?? false : false;
  };
  return runningOf(prev) && !runningOf(next);
}

/**
 * Transcript + activity-log caps (F3, 2026-04-19 freeze diagnosis): both
 * arrays grew without bound, so every streaming delta's agent spread + the
 * transcript re-render got linearly more expensive as a session lengthened
 * — the "reasoning gets slower over time" symptom. Keep the last N entries
 * (a `.slice(-N)` keep-last-N cap), oldest dropped.
 * TRADEOFF: a `/save` conversation export therefore contains only the most
 * recent MAX_TRANSCRIPT_ENTRIES entries of the session.
 */
export const MAX_TRANSCRIPT_ENTRIES = 1000;
export const MAX_ACTIVITY_ENTRIES = 300;

/**
 * Rolling byte budget for image payloads retained in the transcript
 * (mem-perf review LOW 4): the entry-count cap alone let full-size pasted
 * screenshots (~5-10 MB base64 each) accumulate in the zustand store AND the
 * DOM for the whole session. The total data-URL characters across retained
 * `images` arrays is bounded to this budget; overflow evicts the OLDEST
 * entries' payloads (see [`capTranscriptImages`]). 32 MiB ≈ 64-300
 * downscaled attachments (paste-time downscaling caps each at ~0.1-0.5 MB),
 * and still bounds legacy full-size loads at ~3-4 images.
 */
export const MAX_TRANSCRIPT_IMAGE_CHARS = 32 * 1024 * 1024;

/**
 * Keep only the last MAX_TRANSCRIPT_ENTRIES entries (no-op under the cap),
 * then enforce the transcript image byte budget
 * ([`capTranscriptImages`]) on what remains. Every transcript-append site
 * funnels through here, so both caps apply everywhere (direct pushes,
 * reducer events, /load restores).
 */
export function capTranscript(entries: TranscriptEntry[]): TranscriptEntry[] {
  const capped =
    entries.length <= MAX_TRANSCRIPT_ENTRIES
      ? entries
      : entries.slice(-MAX_TRANSCRIPT_ENTRIES);
  return capTranscriptImages(capped);
}

/**
 * Enforce the transcript image byte budget (mem-perf review LOW 4): walk
 * NEWEST→OLDEST keeping image payloads while they fit within
 * [`MAX_TRANSCRIPT_IMAGE_CHARS`]; the first entry that would overflow the
 * budget — and every older image-bearing entry — has its `images` replaced
 * by an `imagesEvicted` count (Message renders a placeholder chip). Pure
 * and identity-preserving: under the budget the SAME array is returned, and
 * untouched entries keep their object references (Message's memoized rows
 * only re-render for actually-evicted entries). Idempotent — evicted
 * entries carry no payload, so re-running changes nothing.
 *
 * A single entry whose images alone exceed the budget is evicted too (the
 * walk hits it immediately), so the retained total is always ≤ budget.
 * Eviction is display- AND export-side: the model's context echo is built
 * server-side and is unaffected, but `/save` serializes the capped
 * transcript — budget-evicted images are permanently absent from saved
 * files (they restore as the placeholder chip). That is the intended
 * trade-off: exporting pre-eviction payloads would reintroduce the
 * unbounded multi-MB save files this budget exists to bound.
 */
export function capTranscriptImages(
  entries: TranscriptEntry[],
): TranscriptEntry[] {
  // Image-bearing kinds: user prompts and landed steers (steered images
  // ride the same payload class, so the same budget applies).
  type ImageBearing = Extract<TranscriptEntry, { kind: "user" | "steer" }> & {
    images: string[];
  };
  const bearsImages = (e: TranscriptEntry): e is ImageBearing =>
    (e.kind === "user" || e.kind === "steer") && e.images != null;
  // Fast path: total the image chars; under budget → nothing to do.
  let total = 0;
  for (const entry of entries) {
    if (bearsImages(entry)) {
      for (const url of entry.images) total += url.length;
    }
  }
  if (total <= MAX_TRANSCRIPT_IMAGE_CHARS) return entries;

  // Over budget: newest-first retention. Find the oldest index whose images
  // still fit; everything image-bearing at or below it is evicted.
  let kept = 0;
  let evictFrom = entries.length;
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i];
    if (!bearsImages(entry)) continue;
    let chars = 0;
    for (const url of entry.images) chars += url.length;
    if (kept + chars <= MAX_TRANSCRIPT_IMAGE_CHARS) {
      kept += chars;
    } else {
      evictFrom = i;
      break;
    }
  }

  // Rebuild, replacing evicted entries' payloads with the placeholder count.
  // Untouched entries keep their references (identity-preserving); an empty
  // `images: []` carries no payload, so it is left alone too.
  return entries.map((entry, i) => {
    if (i > evictFrom || !bearsImages(entry) || entry.images.length === 0) {
      return entry;
    }
    const { images, ...rest } = entry;
    return { ...rest, imagesEvicted: images.length };
  });
}

/**
 * Monotonic transcript-entry id source (mem-perf review HIGH 3): stable
 * React keys for Conversation's turn/run/entry lists. Index keys
 * misattribute row state once the transcript hits its 1000-entry cap —
 * every append shifts all indices, so a Message row's key lands on a
 * DIFFERENT entry (arePropsEqual fails → deep re-render; local row state
 * like an expanded tool card silently transfers). Ids are stamped at
 * creation (the applyAgentEvent identity pass + the direct-push sites)
 * and survive capTranscript's slice, entry spreads, and /save + /load
 * round-trips. Module-lifetime counter — the store is in-memory, so ids
 * only need uniqueness within a session.
 */
let nextEntryId = 1;

/** Allocate the next monotonic transcript-entry id. */
export function allocEntryId(): number {
  return nextEntryId++;
}

/**
 * Stamp any entries lacking an entryId (bulk form — the /load restore
 * path: legacy saved conversations predate the field). Returns the SAME
 * array when every entry already has an id, so a fully-stamped load
 * re-creates nothing. Also syncs the module counter past any foreign ids
 * it passes through: /load is the only path that imports entries with
 * pre-existing entryIds (a saved conversation), and without the sync the
 * next allocation could collide with a loaded id — duplicate React keys,
 * the exact misattribution class this module exists to prevent (review
 * HIGH 1, plan 225e0dad).
 */
export function stampEntryIds(entries: TranscriptEntry[]): TranscriptEntry[] {
  for (const e of entries) {
    if (e.entryId !== undefined && e.entryId >= nextEntryId) {
      nextEntryId = e.entryId + 1;
    }
  }
  if (entries.every((e) => e.entryId !== undefined)) return entries;
  return entries.map((e) =>
    e.entryId === undefined ? { ...e, entryId: allocEntryId() } : e,
  );
}

/** Keep only the last MAX_ACTIVITY_ENTRIES entries (no-op under the cap). */
export function capActivityLog(entries: ActivityEntry[]): ActivityEntry[] {
  return entries.length <= MAX_ACTIVITY_ENTRIES
    ? entries
    : entries.slice(-MAX_ACTIVITY_ENTRIES);
}

/**
 * Whether a transcript entry is an agent-activity card — a tool call, memory
 * read/write, vision image-parsing, or skill announcement — the entry kinds
 * gated by the `show_tool_activity` setting (Settings → Chat → "Show tool
 * activity in chat", default on — tool results show by default; turn the
 * toggle off to hide them). Conversation.tsx skips these at RENDER
 * time when the toggle is off; the transcript store always contains every
 * entry (the model's context echo is built server-side and is unaffected),
 * and the Output tab + console still log every tool call. Conversation
 * entries — user/assistant text, errors, steers, and ask_user Q→A records —
 * always render.
 */
export function isActivityEntry(entry: TranscriptEntry): boolean {
  return (
    entry.kind === "tool" ||
    entry.kind === "memory" ||
    entry.kind === "vision" ||
    entry.kind === "skill"
  );
}

/**
 * Whether a transcript entry is a knowledge-access activity card — a graph_*
 * tool call, a memory tool call, or an auto-recall entry — the subset of
 * `isActivityEntry` gated by the `show_knowledge_activity` setting (Settings
 * → Chat → "Show knowledge activity in chat", default ON). These compact
 * cards surface the agent's knowledge-tool usage (graph lookups, memory
 * reads/writes, auto-recalled context) so the user can see what the agent is
 * accessing without enabling the full `show_tool_activity` toggle (which also
 * reveals shell, read_files, file_edit, and other noisy tool cards).
 *
 * `memory` entries cover BOTH memory tool calls (memory_search,
 * memory_write, …) and auto-recall entries (the `memory_recalled` event
 * pushes a `memory` entry named "auto-recall"). `tool` entries whose name
 * starts with `graph_` cover graph_search / graph_context / graph_impact /
 * graph_path.
 */
export function isKnowledgeActivityEntry(entry: TranscriptEntry): boolean {
  return (
    entry.kind === "memory" ||
    (entry.kind === "tool" && entry.name.startsWith("graph_"))
  );
}

/**
 * Finalize any pending streaming text: move it from `streamingText` into the
 * transcript as an assistant message.
 *
 * The response text lives in the transcript (above the thinking box) AND is
 * mirrored into the activity log as `answer` entries while it streams (user
 * request 2026-08-22 — the thinking box shows reasoning + the live answer).
 * The log is NOT cleared here — it stays visible until new reasoning starts
 * (see the reasoning_delta case).
 *
 * This MUST run before appending any non-text entry (tool call, tool result,
 * error) and at turn end (finished). Otherwise the LLM's text — which streamed
 * *before* the tool call — lands *below* the tool card in the transcript, so
 * tools appear to jump to the top of the conversation.
 */
export function flushStreamingText(s: AgentState): void {
  if (!s.streamingText) return;
  s.transcript = capTranscript([
    ...s.transcript,
    { kind: "assistant", text: s.streamingText },
  ]);
  s.streamingText = "";
}

/**
 * Flush any pending streaming text, then append a transcript entry. Shared by
 * the reducers that inject a display entry from a backend-synthesized event
 * (a backlog-dispatched prompt, a toolbar-started skill, a landed steer) —
 * they all need the in-flight assistant text finalized first so the new entry
 * lands above it, not below. Mutates `agent` in place (the caller spreads it
 * into a new object for the reducer return).
 */
export function pushTranscriptEntry(agent: AgentState, entry: TranscriptEntry): void {
  flushStreamingText(agent);
  agent.transcript = capTranscript([...agent.transcript, entry]);
}

