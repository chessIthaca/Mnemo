// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The Trace tab's data access and payload shaping.
 *
 * The log is one ring on the engine's brain (`TraceApi`, the newest
 * `capacity` requests), and every provider the engine builds records into it —
 * the lead's own turns and every worker session alike, because workers spawn on
 * the lead's engine. A record carries no session field, so rows are told apart
 * by model and endpoint, not by which agent sent them.
 *
 * The list poll is cheap (summaries carry no payloads); one detail — the exact
 * POSTed body and the raw response — is fetched when a row is opened, and only
 * re-fetched when the row's `version` changes.
 */

import { invoke } from "@tauri-apps/api/core";
import { formatEstimate } from "./cost";
import { isTauriRuntime } from "./runtime";

/** Token accounting for one request, as the provider reported it. */
export interface TraceUsage {
  prompt: number;
  completion: number;
  reasoning: number;
  cached: number;
}

/** The client-side stream-guard cut for a response, when one fired. */
export interface TraceGuardCut {
  boundary: string;
  byte_idx: number;
  delta_len: number;
  chars_before: number;
  prefix_empty: boolean;
}

/** One row of the request list: the `LlmRequestSummary` wire record, plus Mnemo2's cost and retention fields. */
export interface TraceRow {
  id: number;
  /** Unix epoch milliseconds at capture time. */
  ts_ms: number;
  model: string;
  base_url: string;
  provider: string;
  http_status: number | null;
  usage: TraceUsage | null;
  finish_reason: string | null;
  /** True once the record is terminal (a finish reason or an error). */
  is_complete: boolean;
  /** Mutation counter; a changed value means the record grew. */
  version: number;
  ttft_ms: number | null;
  generation_ms: number | null;
  reasoning_ms: number | null;
  connect_ms: number | null;
  tools_ms: number | null;
  prep_ms: number | null;
  compact_ms: number | null;
  backoff_ms: number | null;
  stall_ms: number | null;
  error: string | null;
  cancelled: boolean;
  response_truncated: boolean;
  response_evicted: boolean;
  guard_cut: TraceGuardCut | null;
  /** The engine run that recorded this call; the durable key is `run_id:id`. */
  run_id: string;
  /** Who made this call; `null` on records captured before attribution existed. */
  attribution: TraceAttribution | null;
  /** Estimated cost from the shared pricing rule; `null` when the model has no price. */
  cost_micros: number | null;
  /** Whether the pricing rule knew this model (a dash is not zero spend). */
  priced: boolean;
  /** True when only journal metadata survived a restart for this call. */
  retained: boolean;
}

/** One tool call as the model delivered it, before harness normalization. */
export interface TraceToolCall {
  id: string;
  name: string;
  arguments: string;
  truncated: boolean;
}

/** One request in full: the exact POSTed body and the raw response. */
export interface TraceDetail extends TraceRow {
  request_json: unknown;
  response_raw: string | null;
  request_truncated: boolean;
  raw_tool_calls: TraceToolCall[] | null;
}

/** A request's start time as a local wall-clock time (`09:05:07`). */
export function traceClock(tsMs: number): string {
  const at = new Date(tsMs);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}`;
}

/** A token count, shortened once it stops being readable (`1.2k`, `2.5M`). */
export function formatTokens(tokens: number): string {
  if (tokens < 1_000) return String(tokens);
  if (tokens < 1_000_000) {
    return `${(tokens / 1_000).toFixed(tokens < 10_000 ? 1 : 0)}k`;
  }
  return `${(tokens / 1_000_000).toFixed(1)}M`;
}

/** A millisecond duration for a row (`840 ms`, `2.4 s`, `—` when absent). */
export function formatDuration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "—";
  if (ms < 1_000) return `${ms} ms`;
  return `${(ms / 1_000).toFixed(ms < 10_000 ? 1 : 0)} s`;
}

/** How a row's status chip reads. */
export type TraceStatusTone = "ok" | "error" | "live" | "stopped";

/** One row's status: the HTTP status, or how the turn ended. */
export interface TraceStatus {
  label: string;
  tone: TraceStatusTone;
}

/**
 * The status fields `traceStatus` reads. Both row shapes carry them as
 * required: the contract's `TraceRow` and the charts' broader
 * pre-attribution shape, whose other fields are optional. Typing the
 * parameter at what the helper actually reads lets the charts module stay
 * byte-identical across branches.
 */
export interface TraceStatusSource {
  error: string | null;
  cancelled: boolean;
  is_complete: boolean;
  http_status: number | null;
  finish_reason: string | null;
}

/**
 * What the row's status says: a refusal keeps its HTTP status, a cancelled
 * turn reads as cancelled, an in-flight one as streaming, and a clean turn
 * shows its finish reason.
 */
export function traceStatus(row: TraceStatusSource): TraceStatus {
  if (row.error !== null) {
    return {
      label: row.http_status === null ? "failed" : String(row.http_status),
      tone: "error",
    };
  }
  if (row.cancelled) return { label: "cancelled", tone: "stopped" };
  if (!row.is_complete) return { label: "streaming", tone: "live" };
  if (row.http_status !== null && row.http_status >= 400) {
    return { label: String(row.http_status), tone: "error" };
  }
  if (row.finish_reason !== null) return { label: row.finish_reason, tone: "ok" };
  if (row.http_status !== null) return { label: String(row.http_status), tone: "ok" };
  return { label: "done", tone: "ok" };
}

/**
 * How much of one payload the tab shows before offering "Show all" — big
 * enough for a real tool schema or SSE tail, small enough to keep the panel
 * responsive when a request body is hundreds of kilobytes.
 */
export const TRACE_TEXT_CAP = 12_000;

/** One payload clamped for display, with what was kept back. */
export interface CappedText {
  /** The visible part. */
  text: string;
  /** True when `total` exceeds what `text` holds. */
  capped: boolean;
  /** Characters in the whole payload. */
  total: number;
}

/** Clamp one payload for display; never splits a surrogate pair. */
export function capText(text: string, cap: number = TRACE_TEXT_CAP): CappedText {
  if (text.length <= cap) return { text, capped: false, total: text.length };
  let end = cap;
  const last = text.charCodeAt(end - 1);
  if (last >= 0xd800 && last <= 0xdbff) end -= 1;
  return { text: text.slice(0, end), capped: true, total: text.length };
}

/** Pretty-print a JSON value; a plain string passes through unchanged. */
export function prettyJson(value: unknown): string {
  if (value === undefined) return "";
  if (typeof value === "string") return value;
  const text = JSON.stringify(value, null, 2);
  return text === undefined ? String(value) : text;
}

/** One conversation message, flattened for reading. */
export interface TraceMessage {
  role: string;
  text: string;
}

/** A request body split into the sections the tab renders collapsed. */
export interface TraceRequestView {
  /** The system prompt(s), in the order sent. */
  system: string[];
  /** The conversation after the system turn(s). */
  messages: TraceMessage[];
  /** The tool declarations, when the body carried any. */
  tools: { names: string[]; json: string } | null;
  /** The body's remaining fields (model, sampling, stream flag…). */
  params: string;
  /** The whole body, pretty-printed — the fallback when it is not an object. */
  raw: string;
}

/** One message's readable text: content parts flattened, tool calls listed. */
export function messageText(message: unknown): string {
  if (message === null || typeof message !== "object") return prettyJson(message);
  const record = message as Record<string, unknown>;
  const parts: string[] = [];
  const content = record.content;
  if (typeof content === "string") {
    parts.push(content);
  } else if (Array.isArray(content)) {
    for (const part of content) {
      if (typeof part === "string") {
        parts.push(part);
      } else if (part !== null && typeof part === "object") {
        const entry = part as Record<string, unknown>;
        if (typeof entry.text === "string") parts.push(entry.text);
        else if (entry.type === "image_url" || entry.type === "image") parts.push("[image]");
        else if (typeof entry.type === "string") parts.push(`[${entry.type}]`);
      }
    }
  }
  const calls = record.tool_calls;
  if (Array.isArray(calls)) {
    for (const call of calls) {
      if (call === null || typeof call !== "object") continue;
      const fn = (call as Record<string, unknown>).function;
      if (fn === null || typeof fn !== "object") continue;
      const { name, arguments: args } = fn as Record<string, unknown>;
      parts.push(
        `→ ${typeof name === "string" ? name : "tool"}(${typeof args === "string" ? args : ""})`,
      );
    }
  }
  return parts.join("\n");
}

/** Split one recorded request into the pieces the detail view shows. */
export function requestView(detail: TraceDetail): TraceRequestView {
  const raw = prettyJson(detail.request_json);
  const body = detail.request_json;
  if (body === null || typeof body !== "object" || Array.isArray(body)) {
    return { system: [], messages: [], tools: null, params: "", raw };
  }
  const { messages, tools, ...params } = body as Record<string, unknown>;
  const system: string[] = [];
  const conversation: TraceMessage[] = [];
  for (const message of Array.isArray(messages) ? messages : []) {
    const role =
      message !== null && typeof message === "object"
        ? (message as Record<string, unknown>).role
        : null;
    const text = messageText(message);
    if (role === "system") system.push(text);
    else conversation.push({ role: typeof role === "string" ? role : "?", text });
  }
  let declarations: TraceRequestView["tools"] = null;
  if (Array.isArray(tools) && tools.length > 0) {
    const names = tools.map((tool, index) => {
      const fn =
        tool !== null && typeof tool === "object"
          ? (tool as Record<string, unknown>).function
          : null;
      const name =
        fn !== null && typeof fn === "object"
          ? (fn as Record<string, unknown>).name
          : null;
      return typeof name === "string" ? name : `tool ${index + 1}`;
    });
    declarations = { names, json: prettyJson(tools) };
  }
  const rest = Object.keys(params).length > 0 ? prettyJson(params) : "";
  return { system, messages: conversation, tools: declarations, params: rest, raw };
}

/** One response's readable text; evicted payloads come back as empty. */
export function responseText(detail: TraceDetail): string {
  if (detail.response_evicted) return "";
  return detail.response_raw ?? "";
}

// ---------------------------------------------------------------------------
// The Trace route's contract — the full-page view of every model call.
//
// The wire shape is frozen in the plan's Interfaces section: Rust keeps
// snake_case serde, this file mirrors it exactly, and the commands below are
// what the route and the compact Workbench tab both use since step E switched
// the tab over (`trace_list` takes the query, `trace_detail` a durable key,
// and `trace_settings`/`trace_export`/`trace_clear_live`/
// `trace_delete_history` back the data controls).
// ---------------------------------------------------------------------------

/** The purpose a call was made for, as the kernel stamps it. */
export type TracePurpose = "turn" | "compaction" | "memory_consolidation" | "routing" | "other";

/** The actor kinds attribution can name. */
export type TraceActorKind = "lead" | "worker" | "subagent" | "system";

/** Who made one model call; the kernel stamps this on every recorded request. */
export interface TraceAttribution {
  actor_kind: TraceActorKind;
  agent_id: string;
  session_id: string;
  agent_name: string;
  worker_id: string | null;
  plan_id: string | null;
  plan_step: number | null;
  turn_id: string | null;
  purpose: TracePurpose;
}

/** Status classes the list can filter on; Waiting and Streaming both count as in progress. */
export type TraceListStatus = "complete" | "in_progress" | "failed" | "cancelled" | "guard_cut";

/** Where the debounced content search looks. */
export type TraceSearchScope = "all" | "request" | "response" | "reasoning" | "tools" | "metadata";

/** The filter set `trace_list` accepts. */
export interface TraceListQuery {
  /** "lead", a worker id, an agent id, or "unattributed". */
  agent?: string;
  model?: string;
  status?: TraceListStatus;
  purpose?: TracePurpose;
  from_ms?: number;
  to_ms?: number;
  turn_id?: string;
  search?: string;
  search_scope?: TraceSearchScope;
  cursor?: string;
  /** Page size cap: the backend clamps anything above 200. */
  limit?: number;
}

/** One page of `trace_list`. */
export interface TraceListPage {
  rows: TraceRow[];
  /** How many calls match the filters, beyond the rows this page carries. */
  total: number;
  /** How many matching calls have no configured price. */
  unpriced: number;
  has_more: boolean;
  next_cursor: string | null;
  /** The backend capped the result set; narrow the filters to see the rest. */
  capped: boolean;
}

/** What `trace_settings` reports about on-disk trace logging. */
export interface TraceSettings {
  log_bodies: boolean;
  ring_capacity: number;
  journal_bytes: number;
  history_bytes: number;
}

/** `trace_delete_history`'s outcome, for the confirmation sheet. */
export interface TraceHistoryDeletion {
  deleted_files: number;
  bytes: number;
}

/** `trace_export`'s scope: one call, a hand-picked set, or the current filters. */
export interface TraceExportRequest {
  scope: "call" | "selected" | "filtered";
  /** Durable `run_id:id` keys, for scope "selected". */
  keys?: string[];
  query?: TraceListQuery;
}

/** `trace_export`'s outcome. */
export interface TraceExportResult {
  path: string;
  count: number;
  includes_bodies: boolean;
}

/** The IPC surface the Trace route needs. */
export interface TracePageTransport {
  /** One filtered page of the log, newest first. */
  list(query: TraceListQuery): Promise<TraceListPage>;
  /** One request in full by its durable key, or `null` when it was not retained. */
  detail(runId: string, id: number): Promise<TraceDetail | null>;
  settings(): Promise<TraceSettings>;
  setLogBodies(on: boolean): Promise<TraceSettings>;
  /** Drop every live ring record. */
  clearLive(): Promise<void>;
  /** Drop the retained metadata journal. */
  deleteHistory(): Promise<TraceHistoryDeletion>;
  exportCalls(request: TraceExportRequest): Promise<TraceExportResult>;
  /**
   * Show one exported bundle in the OS file manager. The backend accepts only
   * paths inside `.coding/logs/trace-exports/`, so a stale result can never
   * point the app at an arbitrary file.
   */
  revealExport(path: string): Promise<void>;
}

/** The dev-only preview's stand-in page transport; never set in production. */
let pagePreviewSeed: TracePageTransport | null = null;

/**
 * Point the Trace route at fixture data for the dev-only preview page
 * (`npm run check:no-mock` keeps this out of the production entry).
 */
export function seedTracePagePreview(transport: TracePageTransport | null): void {
  pagePreviewSeed = transport;
}

/** Build the Trace route's transport, or `null` outside the desktop app. */
export function createTracePageTransport(): TracePageTransport | null {
  if (pagePreviewSeed) return pagePreviewSeed;
  if (!isTauriRuntime()) return null;
  return {
    list: (query) => invoke<TraceListPage>("trace_list", { query }),
    detail: (runId, id) => invoke<TraceDetail | null>("trace_detail", { runId, id }),
    settings: () => invoke<TraceSettings>("trace_settings"),
    setLogBodies: (on) => invoke<TraceSettings>("trace_set_log_bodies", { on }),
    clearLive: () => invoke<void>("trace_clear_live"),
    deleteHistory: () => invoke<TraceHistoryDeletion>("trace_delete_history"),
    exportCalls: (request) => invoke<TraceExportResult>("trace_export", { request }),
    revealExport: (path) => invoke<void>("trace_reveal_export", { path }),
  };
}

/** How many rows one list page asks for; the backend clamps at 200. */
export const TRACE_PAGE_SIZE = 100;

/**
 * A row's durable key — the plan froze `run_id:id` because request ids
 * restart with the engine process.
 */
export function traceKey(row: Pick<TraceRow, "run_id" | "id">): string {
  return `${row.run_id}:${row.id}`;
}
// ---------------------------------------------------------------------------
// Status, attribution and purpose — the friendly labels of §7.
// ---------------------------------------------------------------------------

/** How a status renders: the token classes the row chips use. */
export type CallStatusTone = "ok" | "live" | "error" | "stopped" | "guard";

/** One call's status, derived the same way for rows and details. */
export interface CallStatus {
  /** The query's status class, so filters and chips agree. */
  key: TraceListStatus;
  /** The friendly label. */
  label: string;
  /** The chip tone. */
  tone: CallStatusTone;
}

/**
 * The status a call is in: Waiting and Streaming are the two faces of
 * in-progress (headers not yet arrived vs. chunks flowing), and a client
 * guard cut is its own terminal class, distinct from failure.
 */
export function callStatus(row: TraceRow): CallStatus {
  if (row.error !== null) return { key: "failed", label: "Failed", tone: "error" };
  if (row.cancelled) return { key: "cancelled", label: "Cancelled", tone: "stopped" };
  if (row.guard_cut !== null) return { key: "guard_cut", label: "Guard cut", tone: "guard" };
  if (!row.is_complete) {
    return {
      key: "in_progress",
      label: row.ttft_ms === null ? "Waiting" : "Streaming",
      tone: "live",
    };
  }
  return { key: "complete", label: "Complete", tone: "ok" };
}

/** The kind an actor label falls back to when no name was captured. */
export function actorKindLabel(kind: TraceActorKind): string {
  switch (kind) {
    case "lead":
      return "Lead";
    case "subagent":
      return "Subagent";
    case "system":
      return "System";
    default:
      return "Worker";
  }
}

/**
 * Who made the call, as the row shows it: the lead reads as "Lead", workers
 * and subagents keep the name captured at call time (deleted workers stay
 * labelled), and a record without attribution never borrows the lead's name.
 */
export function actorLabel(attribution: TraceAttribution | null): string {
  if (attribution === null) return "Unattributed";
  if (attribution.actor_kind === "lead") return "Lead";
  if (attribution.agent_name.trim() !== "") return attribution.agent_name;
  return actorKindLabel(attribution.actor_kind);
}

/** The worker's plan line, as the row's second line shows it. */
export function actorSubLabel(attribution: TraceAttribution | null): string {
  if (attribution === null || attribution.actor_kind !== "worker") return "";
  const parts: string[] = [];
  if (attribution.plan_id !== null) parts.push(`Plan ${attribution.plan_id}`);
  // Plan steps are 0-based in the team ledger, like `Worker.planStep`.
  if (attribution.plan_step !== null) parts.push(`Step ${attribution.plan_step + 1}`);
  return parts.join(" · ");
}

/** One purpose's friendly label, for rows and the purpose filter. */
export function purposeLabel(purpose: TracePurpose): string {
  switch (purpose) {
    case "turn":
      return "Conversation turn";
    case "compaction":
      return "Compaction";
    case "memory_consolidation":
      return "Memory consolidation";
    case "routing":
      return "Routing";
    default:
      return "Other";
  }
}

/** The purpose filter's options, in the design's fixed order. */
export const TRACE_PURPOSE_OPTIONS: { value: TracePurpose; label: string }[] = [
  { value: "turn", label: "Conversation turn" },
  { value: "compaction", label: "Compaction" },
  { value: "memory_consolidation", label: "Memory consolidation" },
  { value: "routing", label: "Routing" },
  { value: "other", label: "Other" },
];

/** A turn id short enough for a chip, with the full value on hover. */
export function shortTurnId(turnId: string): string {
  return turnId.length <= 10 ? turnId : `${turnId.slice(0, 4)}…${turnId.slice(-4)}`;
}

/** A start time with milliseconds, for the detail header (`14:32:08.114`). */
export function traceClockMs(tsMs: number): string {
  const at = new Date(tsMs);
  const ms = String(at.getMilliseconds()).padStart(3, "0");
  return `${traceClock(tsMs)}.${ms}`;
}

/** How long ago one call started, for hover text. */
export function traceAge(tsMs: number, nowMs: number): string {
  const seconds = Math.max(0, Math.round((nowMs - tsMs) / 1_000));
  if (seconds < 45) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} minute${minutes === 1 ? "" : "s"} ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.round(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}

/** A local day key, so grouping buckets by wall-clock day. */
export function traceDayKey(tsMs: number): string {
  const at = new Date(tsMs);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`;
}

const DAY_LABEL = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" });

/** A day's group title: Today, Yesterday, or the month-day form. */
export function traceDayLabel(dayKey: string, nowMs: number): string {
  if (dayKey === traceDayKey(nowMs)) return "Today";
  if (dayKey === traceDayKey(nowMs - 86_400_000)) return "Yesterday";
  const at = new Date(`${dayKey}T12:00:00`);
  return Number.isNaN(at.getTime()) ? dayKey : DAY_LABEL.format(at);
}

/** Why the model stopped, translated where the reason is known. */
export function finishLabel(reason: string | null): string | null {
  if (reason === null) return null;
  switch (reason) {
    case "stop":
      return "Completed";
    case "tool_calls":
      return "Tool calls";
    case "length":
      return "Length limit";
    case "content_filter":
      return "Content filter";
    default:
      return reason;
  }
}// ---------------------------------------------------------------------------
// Tokens, cost and timing — the numbers a row and the Overview show.
// ---------------------------------------------------------------------------

/**
 * The share of input tokens the provider reported as cached, 0..1 — or
 * `null` when no usage was reported ("not reported" is not 0%).
 */
export function cacheReuse(usage: TraceUsage | null): number | null {
  if (usage === null || usage.prompt <= 0) return null;
  return Math.min(1, usage.cached / usage.prompt);
}

/** Cache reuse as the row's text: `92% cached`, or `cache n/a`. */
export function cacheReuseLabel(usage: TraceUsage | null): string {
  const reuse = cacheReuse(usage);
  return reuse === null ? "cache n/a" : `${Math.round(reuse * 100)}% cached`;
}

/** One call's cost text: the shared pricing rule's estimate, `—` when unpriced. */
export function traceCostLabel(costMicros: number | null): string {
  return formatEstimate(costMicros);
}

/** The tokens one row shows: input and output, not counting reasoning twice. */
export function rowTokenSummary(row: TraceRow): { input: number | null; output: number | null } {
  if (row.usage === null) return { input: null, output: null };
  return { input: row.usage.prompt, output: row.usage.completion };
}

/** One timing phase, with the sub-phases counted inside it. */
export interface TracePhase {
  /** The glossary key the label's tip explains. */
  key: MetricTerm;
  label: string;
  ms: number | null;
  /** Phases measured inside `ms` — Compaction in Setup, Stalls in Generation. */
  inside?: TracePhase[];
}

/**
 * The request's timing phases, in the order the wire runs them. Total time
 * is their sum; `Tools` runs after the response and never counts towards it.
 */
export function tracePhases(row: TraceRow): TracePhase[] {
  const phases: TracePhase[] = [];
  const setupInside: TracePhase[] | undefined =
    row.compact_ms === null ? undefined : [{ key: "compaction", label: "Compaction", ms: row.compact_ms }];
  phases.push({ key: "setup", label: "Setup", ms: row.prep_ms, inside: setupInside });
  phases.push({ key: "connect", label: "Connect", ms: row.connect_ms });
  phases.push({ key: "first_token", label: "First token", ms: row.ttft_ms });
  const generationInside: TracePhase[] | undefined =
    row.stall_ms === null ? undefined : [{ key: "stalls", label: "Stalls", ms: row.stall_ms }];
  phases.push({ key: "generation", label: "Generation", ms: row.generation_ms, inside: generationInside });
  phases.push({ key: "retry_wait", label: "Retry wait", ms: row.backoff_ms });
  return phases;
}

/** Wall time for one call: the sum of the phases that were captured, or `null`. */
export function phaseTotalMs(row: TraceRow): number | null {
  let total: number | null = null;
  for (const phase of tracePhases(row)) {
    if (phase.ms !== null) total = (total ?? 0) + phase.ms;
  }
  return total;
}// ---------------------------------------------------------------------------
// Readable content — what the Response and Context tabs render.
// ---------------------------------------------------------------------------

/** One response, split the way the Response tab reads it. */
export interface TraceResponseView {
  /** The reasoning the model streamed, when the provider reports it. */
  reasoning: string;
  /** The answer text, assembled from the streamed chunks. */
  answer: string;
  /** The provider's error envelope message, when the body carries one. */
  errorText: string | null;
  /** True when the body was evicted or never captured. */
  absent: boolean;
  /** True when the text came from an SSE stream rather than one JSON body. */
  streamed: boolean;
}

const EMPTY_RESPONSE: TraceResponseView = {
  reasoning: "",
  answer: "",
  errorText: null,
  absent: true,
  streamed: false,
};

/**
 * Turn one recorded response into readable text: SSE chunks accumulate into
 * Reasoning and Answer, a JSON error envelope becomes its message, and an
 * evicted or missing body says so instead of rendering as empty content.
 */
export function responseView(detail: TraceDetail): TraceResponseView {
  const raw = detail.response_raw;
  if (raw === null || detail.response_evicted) return EMPTY_RESPONSE;
  const trimmed = raw.trim();
  if (trimmed === "") return EMPTY_RESPONSE;

  // A single JSON body: an error envelope, or a non-streaming completion.
  if (trimmed.startsWith("{")) {
    try {
      const body = JSON.parse(trimmed) as Record<string, unknown>;
      const error = body.error;
      if (error !== null && typeof error === "object" && typeof (error as { message?: unknown }).message === "string") {
        return { ...EMPTY_RESPONSE, errorText: (error as { message: string }).message, absent: false };
      }
      const choices = Array.isArray(body.choices) ? (body.choices[0] as Record<string, unknown> | undefined) : undefined;
      const message = choices?.message;
      if (message !== null && typeof message === "object") {
        const record = message as Record<string, unknown>;
        const content = typeof record.content === "string" ? record.content : "";
        const reasoning =
          typeof record.reasoning_content === "string"
            ? record.reasoning_content
            : typeof record.reasoning === "string"
              ? record.reasoning
              : "";
        return { reasoning, answer: content, errorText: null, absent: false, streamed: false };
      }
      return { ...EMPTY_RESPONSE, absent: false };
    } catch {
      return { ...EMPTY_RESPONSE, answer: trimmed, absent: false };
    }
  }

  // An SSE stream: accumulate the deltas chunk by chunk.
  let reasoning = "";
  let answer = "";
  let errorText: string | null = null;
  let streamed = false;
  for (const line of raw.split("\n")) {
    const text = line.trim();
    if (!text.startsWith("data:")) continue;
    const payload = text.slice(5).trim();
    if (payload === "" || payload === "[DONE]") continue;
    try {
      const chunk = JSON.parse(payload) as Record<string, unknown>;
      if (
        errorText === null &&
        chunk.error !== null &&
        typeof chunk.error === "object" &&
        typeof (chunk.error as { message?: unknown }).message === "string"
      ) {
        errorText = (chunk.error as { message: string }).message;
      }
      const choices = Array.isArray(chunk.choices) ? (chunk.choices[0] as Record<string, unknown> | undefined) : undefined;
      const delta = choices?.delta;
      if (delta !== null && typeof delta === "object") {
        const record = delta as Record<string, unknown>;
        if (typeof record.content === "string") answer += record.content;
        const reasoningPart =
          typeof record.reasoning_content === "string"
            ? record.reasoning_content
            : typeof record.reasoning === "string"
              ? record.reasoning
              : null;
        if (reasoningPart !== null) reasoning += reasoningPart;
      }
      streamed = true;
    } catch {
      // A malformed chunk is skipped; the raw text stays one tab away.
    }
  }
  if (!streamed) return { ...EMPTY_RESPONSE, answer: trimmed, absent: false };
  return { reasoning, answer, errorText, absent: false, streamed: true };
}

/** One readable block of a context message. */
export interface TraceMessageBlock {
  kind: "text" | "image" | "tool";
  /** The block's readable text. */
  text: string;
}

/**
 * A message's readable content: content arrays become separate text and
 * image blocks instead of serialized part noise, and tool requests read
 * as `→ name(arguments)`.
 */
export function messageBlocks(message: unknown): TraceMessageBlock[] {
  if (message === null || typeof message !== "object") {
    return [{ kind: "text", text: prettyJson(message) }];
  }
  const record = message as Record<string, unknown>;
  const blocks: TraceMessageBlock[] = [];
  const content = record.content;
  if (typeof content === "string") {
    blocks.push({ kind: "text", text: content });
  } else if (Array.isArray(content)) {
    for (const part of content) {
      if (part !== null && typeof part === "object") {
        const partRecord = part as Record<string, unknown>;
        if (partRecord.type === "text" && typeof partRecord.text === "string") {
          blocks.push({ kind: "text", text: partRecord.text });
        } else if (partRecord.type === "image_url" || partRecord.type === "input_image" || partRecord.type === "image") {
          blocks.push({ kind: "image", text: "[image]" });
        } else if (partRecord.type === "tool_use") {
          blocks.push({ kind: "tool", text: `→ ${String(partRecord.name ?? "tool")}(${JSON.stringify(partRecord.input ?? null)})` });
        } else {
          blocks.push({ kind: "text", text: prettyJson(part) });
        }
      } else if (typeof part === "string") {
        blocks.push({ kind: "text", text: part });
      }
    }
  }
  const toolCalls = record.tool_calls;
  if (Array.isArray(toolCalls)) {
    for (const call of toolCalls) {
      if (call === null || typeof call !== "object") continue;
      const callRecord = call as Record<string, unknown>;
      const fn =
        callRecord.function !== null && typeof callRecord.function === "object"
          ? (callRecord.function as Record<string, unknown>)
          : null;
      const name = typeof fn?.name === "string" ? fn.name : "tool";
      const args = typeof fn?.arguments === "string" ? fn.arguments : JSON.stringify(fn?.arguments ?? null);
      blocks.push({ kind: "tool", text: `→ ${name}(${args})` });
    }
  }
  if (blocks.length === 0) blocks.push({ kind: "text", text: prettyJson(message) });
  return blocks;
}// ---------------------------------------------------------------------------
// Grouping — Time, Agent and Turn, per §4.
// ---------------------------------------------------------------------------

/** The list's grouping modes. */
export type TraceGrouping = "time" | "agent" | "turn";

/** One rendered group of rows. */
export interface TraceRowGroup {
  /** Unique group key for keyed rendering. */
  key: string;
  /** The group's heading. */
  title: string;
  /** The aggregate line: calls, span, tokens, cost. */
  meta: string;
  /** The group's rows, in display order. */
  rows: TraceRow[];
  /** Per-turn sections inside an agent group. */
  turns?: TraceRowGroup[];
}

/** Aggregate a group header's meta line over its rows. */
function groupMeta(rows: TraceRow[], span: boolean): string {
  const calls = rows.length === 1 ? "1 call" : `${rows.length} calls`;
  let input = 0;
  let output = 0;
  let costMicros: number | null = null;
  let unpriced = false;
  for (const row of rows) {
    if (row.usage !== null) {
      input += row.usage.prompt;
      output += row.usage.completion;
    }
    if (row.cost_micros !== null) costMicros = (costMicros ?? 0) + row.cost_micros;
    if (!row.priced) unpriced = true;
  }
  const parts = [calls];
  if (span && rows.length > 1) {
    parts.push(`${traceClock(rows[rows.length - 1].ts_ms)}–${traceClock(rows[0].ts_ms)}`);
  }
  if (input > 0 || output > 0) parts.push(`${formatTokens(input)} in · ${formatTokens(output)} out`);
  parts.push(costMicros === null ? "—" : `${formatEstimate(costMicros)}${unpriced ? "+" : ""}`);
  return parts.join(" · ");
}

function makeGroup(key: string, title: string, rows: TraceRow[], span: boolean, turns?: TraceRowGroup[]): TraceRowGroup {
  const group: TraceRowGroup = { key, title, meta: groupMeta(rows, span), rows, turns };
  return group;
}

/**
 * Group the loaded rows. `rows` arrive newest first; Time keeps that order
 * inside day buckets, while Agent and Turn list each group's calls
 * chronologically — a turn reads as a story, not a countdown.
 */
export function groupTraceRows(rows: TraceRow[], grouping: TraceGrouping, nowMs: number): TraceRowGroup[] {
  if (grouping === "time") {
    const groups: TraceRowGroup[] = [];
    for (const row of rows) {
      const dayKey = traceDayKey(row.ts_ms);
      const last = groups[groups.length - 1];
      if (last !== undefined && last.key === dayKey) {
        last.rows.push(row);
        last.meta = groupMeta(last.rows, false);
      } else groups.push(makeGroup(dayKey, traceDayLabel(dayKey, nowMs), [row], false));
    }
    return groups;
  }

  if (grouping === "turn") {
    const buckets = new Map<string, TraceRow[]>();
    for (const row of rows) {
      const key = row.attribution?.turn_id ?? "standalone";
      const bucket = buckets.get(key);
      if (bucket !== undefined) bucket.push(row);
      else buckets.set(key, [row]);
    }
    const groups: TraceRowGroup[] = [];
    for (const [key, bucket] of buckets) {
      bucket.sort((a, b) => a.ts_ms - b.ts_ms || a.id - b.id);
      const actors = [...new Set(bucket.map((row) => actorLabel(row.attribution)))].join(", ");
      const title = key === "standalone" ? "Standalone calls" : `Turn ${shortTurnId(key)}`;
      groups.push(makeGroup(`turn:${key}`, `${title} — ${actors}`, bucket, true));
    }
    // Newest groups first; within a group, chronological.
    groups.sort((a, b) => b.rows[b.rows.length - 1].ts_ms - a.rows[a.rows.length - 1].ts_ms);
    return groups;
  }

  // Agent: group by actor, each holding that actor's turns in order.
  const actors = new Map<string, TraceRow[]>();
  for (const row of rows) {
    const key = row.attribution === null ? "unattributed" : row.attribution.agent_id;
    const bucket = actors.get(key);
    if (bucket !== undefined) bucket.push(row);
    else actors.set(key, [row]);
  }
  const groups: TraceRowGroup[] = [];
  for (const [key, bucket] of actors) {
    bucket.sort((a, b) => a.ts_ms - b.ts_ms || a.id - b.id);
    const turns: TraceRowGroup[] = [];
    let lastTurnKey: string | null = null;
    for (const row of bucket) {
      const turnKey = row.attribution?.turn_id ?? "standalone";
      const last = turns[turns.length - 1];
      if (last !== undefined && lastTurnKey === turnKey) {
        last.rows.push(row);
        last.meta = groupMeta(last.rows, false);
      } else {
        turns.push(
          makeGroup(
            `agent:${key}:turn:${turnKey}`,
            turnKey === "standalone" ? "Standalone calls" : `Turn ${shortTurnId(turnKey)}`,
            [row],
            false,
          ),
        );
        lastTurnKey = turnKey;
      }
    }
    for (const turn of turns) turn.meta = groupMeta(turn.rows, false);
    groups.push(makeGroup(`agent:${key}`, actorLabel(bucket[0].attribution), bucket, true, turns));
  }
  groups.sort((a, b) => b.rows[b.rows.length - 1].ts_ms - a.rows[a.rows.length - 1].ts_ms);
  return groups;
}

// ---------------------------------------------------------------------------
// Prefix reuse — the explained comparison against the previous call.
// ---------------------------------------------------------------------------

/** The prefix comparison between two calls' request bodies. */
export interface TracePrefixReuse {
  /** 0..1 of the current request's characters that match the previous request's start. */
  pct: number;
  sameChars: number;
  totalChars: number;
  addedChars: number;
}

/** The request body's message list, whatever shape the provider's body used. */
function requestBodyMessages(request: unknown): unknown[] | null {
  if (request === null || typeof request !== "object") return null;
  const messages = (request as Record<string, unknown>).messages;
  return Array.isArray(messages) ? messages : null;
}

/**
 * The newest earlier call by the same agent, purpose and model — the one
 * the prefix comparison measures against.
 */
export function previousComparableRow(rows: TraceRow[], current: TraceRow): TraceRow | null {
  const currentAttr = current.attribution;
  const agent = currentAttr === null ? null : currentAttr.agent_id;
  for (const row of rows) {
    if (row.id === current.id && row.run_id === current.run_id) continue;
    if (row.ts_ms > current.ts_ms || (row.ts_ms === current.ts_ms && row.id >= current.id)) continue;
    const rowAttr = row.attribution;
    const rowAgent = rowAttr === null ? null : rowAttr.agent_id;
    if (rowAgent !== agent) continue;
    if (rowAttr?.purpose !== currentAttr?.purpose) continue;
    if (row.model !== current.model) continue;
    return row;
  }
  return null;
}

function commonPrefixLength(a: string, b: string): number {
  const limit = Math.min(a.length, b.length);
  let index = 0;
  while (index < limit && a.charCodeAt(index) === b.charCodeAt(index)) index += 1;
  return index;
}

/**
 * How much of this request's text repeats the previous comparable request
 * from its first character. Bodies that were truncated or not retained do
 * not support the comparison — the caller explains why instead of guessing.
 */
export function prefixReuse(current: TraceDetail, previous: TraceDetail | null): TracePrefixReuse | null {
  if (previous === null) return null;
  if (current.request_truncated || previous.request_truncated) return null;
  const currentMessages = requestBodyMessages(current.request_json);
  const previousMessages = requestBodyMessages(previous.request_json);
  if (currentMessages === null || previousMessages === null) return null;
  if (currentMessages.length === 0 || previousMessages.length === 0) return null;
  const currentText = JSON.stringify(currentMessages);
  const previousText = JSON.stringify(previousMessages);
  const sameChars = commonPrefixLength(currentText, previousText);
  const totalChars = currentText.length;
  if (totalChars === 0) return null;
  return {
    pct: sameChars / totalChars,
    sameChars,
    totalChars,
    addedChars: totalChars - sameChars,
  };
}// ---------------------------------------------------------------------------
// Filters, query building and options.
// ---------------------------------------------------------------------------

/** The Time filter's presets. */
export type TraceTimeKey = "session" | "hour" | "day" | "all" | "custom";

/** Every filter the Trace route's toolbar carries. */
export interface TraceFilters {
  /** "all", "lead", a worker/agent id, or "unattributed". */
  agent: string;
  /** "all" or a model name. */
  model: string;
  /** "all" or one status class. */
  status: "all" | TraceListStatus;
  /** "all" or one purpose. */
  purpose: "all" | TracePurpose;
  time: TraceTimeKey;
  /** Custom range bounds, epoch ms; used when `time` is "custom". */
  from_ms: number | null;
  to_ms: number | null;
  search: string;
  scope: TraceSearchScope;
  grouping: TraceGrouping;
  /** Turn hand-over from an entry point; a chip removes it. */
  turn_id: string | null;
  /** The durable key of the open call ("run_id:id"), restored on return. */
  selected: string | null;
}

/** How the route opens: everything, this session, newest first. */
export const DEFAULT_TRACE_FILTERS: TraceFilters = {
  agent: "all",
  model: "all",
  status: "all",
  purpose: "all",
  time: "session",
  from_ms: null,
  to_ms: null,
  search: "",
  scope: "all",
  grouping: "time",
  turn_id: null,
  selected: null,
};

/** The Time presets the toolbar offers, in the design's order. */
export const TRACE_TIME_OPTIONS: { value: TraceTimeKey; label: string }[] = [
  { value: "session", label: "This session" },
  { value: "hour", label: "Last hour" },
  { value: "day", label: "Last 24 hours" },
  { value: "all", label: "All retained" },
  { value: "custom", label: "Custom range" },
];

/** The time range a filter set translates to, epoch ms. */
export function traceTimeRange(filters: TraceFilters, sessionStartMs: number, nowMs: number): { from_ms?: number; to_ms?: number } {
  switch (filters.time) {
    case "session":
      return { from_ms: sessionStartMs };
    case "hour":
      return { from_ms: nowMs - 3_600_000 };
    case "day":
      return { from_ms: nowMs - 86_400_000 };
    case "custom":
      return {
        ...(filters.from_ms !== null ? { from_ms: filters.from_ms } : {}),
        ...(filters.to_ms !== null ? { to_ms: filters.to_ms } : {}),
      };
    default:
      return {};
  }
}

/**
 * Build the wire query from the toolbar's filter state. "all" values are
 * simply omitted — the backend treats an absent filter as unrestricted.
 */
export function traceListQuery(
  filters: TraceFilters,
  options: { sessionStartMs: number; nowMs: number; cursor?: string | null; limit?: number },
): TraceListQuery {
  const query: TraceListQuery = {};
  if (filters.agent !== "all") query.agent = filters.agent;
  if (filters.model !== "all") query.model = filters.model;
  if (filters.status !== "all") query.status = filters.status;
  if (filters.purpose !== "all") query.purpose = filters.purpose;
  if (filters.turn_id !== null) query.turn_id = filters.turn_id;
  const range = traceTimeRange(filters, options.sessionStartMs, options.nowMs);
  if (range.from_ms !== undefined) query.from_ms = range.from_ms;
  if (range.to_ms !== undefined) query.to_ms = range.to_ms;
  const search = filters.search.trim();
  if (search !== "") {
    query.search = search;
    query.search_scope = filters.scope;
  }
  if (options.cursor !== null && options.cursor !== undefined) query.cursor = options.cursor;
  if (options.limit !== undefined) query.limit = options.limit;
  return query;
}

/** The status classes' friendly filter labels, shared by the select and chips. */
export const TRACE_STATUS_OPTIONS: { value: TraceListStatus; label: string }[] = [
  { value: "complete", label: "Complete" },
  { value: "in_progress", label: "In progress" },
  { value: "failed", label: "Errors only" },
  { value: "cancelled", label: "Cancelled" },
  { value: "guard_cut", label: "Guard cut" },
];

/** The filters that differ from the defaults, as removable chips. */
export function activeTraceChips(filters: TraceFilters): { key: keyof TraceFilters; label: string }[] {
  const chips: { key: keyof TraceFilters; label: string }[] = [];
  if (filters.agent !== "all") chips.push({ key: "agent", label: `Agent: ${filters.agent}` });
  if (filters.model !== "all") chips.push({ key: "model", label: `Model: ${filters.model}` });
  if (filters.status !== "all") {
    const label = TRACE_STATUS_OPTIONS.find((option) => option.value === filters.status)?.label ?? filters.status;
    chips.push({ key: "status", label: `Status: ${label}` });
  }
  if (filters.purpose !== "all") {
    chips.push({ key: "purpose", label: `Purpose: ${purposeLabel(filters.purpose)}` });
  }
  if (filters.time === "custom") {
    const from = filters.from_ms === null ? "…" : new Date(filters.from_ms).toISOString();
    const to = filters.to_ms === null ? "…" : new Date(filters.to_ms).toISOString();
    chips.push({ key: "time", label: `Time: ${from} → ${to}` });
  } else if (filters.time !== "session") {
    chips.push({ key: "time", label: TRACE_TIME_OPTIONS.find((option) => option.value === filters.time)?.label ?? "Time" });
  }
  if (filters.search.trim() !== "") chips.push({ key: "search", label: `Search: ${filters.search.trim()}` });
  if (filters.turn_id !== null) chips.push({ key: "turn_id", label: `Turn: ${shortTurnId(filters.turn_id)}` });
  return chips;
}

/** One option of the Agent filter. */
export interface TraceAgentOption {
  value: string;
  label: string;
}

/**
 * The Agent filter's options: All, the Lead, every live worker, then every
 * other actor observed in the log (deleted workers keep the name captured on
 * their calls), and Unattributed last.
 */
export function traceAgentOptions(rows: TraceRow[], workers: { id: string; name: string }[] = []): TraceAgentOption[] {
  const options: TraceAgentOption[] = [
    { value: "all", label: "All agents" },
    { value: "lead", label: "Lead" },
  ];
  const seen = new Set(["all", "lead"]);
  for (const worker of workers) {
    if (worker.id === "" || seen.has(worker.id)) continue;
    seen.add(worker.id);
    options.push({ value: worker.id, label: worker.name });
  }
  for (const row of rows) {
    const attribution = row.attribution;
    if (attribution === null || attribution.actor_kind === "lead" || attribution.actor_kind === "system") continue;
    const value = attribution.worker_id ?? attribution.agent_id;
    if (value === "" || seen.has(value)) continue;
    seen.add(value);
    options.push({ value, label: actorLabel(attribution) });
  }
  options.push({ value: "unattributed", label: "Unattributed" });
  return options;
}

/** One option of the Model filter. */
export interface TraceModelOption {
  value: string;
  label: string;
}

/** The Model filter's options: every observed model, with the provider when a name is ambiguous. */
export function traceModelOptions(rows: TraceRow[]): TraceModelOption[] {
  const providers = new Map<string, Set<string>>();
  for (const row of rows) {
    const set = providers.get(row.model) ?? new Set<string>();
    set.add(row.provider);
    providers.set(row.model, set);
  }
  const options: TraceModelOption[] = [{ value: "all", label: "All models" }];
  for (const [model, set] of providers) {
    const names = [...set];
    options.push({ value: model, label: names.length > 1 ? `${model} (${names.join(", ")})` : model });
  }
  return options;
}

// ---------------------------------------------------------------------------
// The glossary — every hover explanation §7 asks for, keyboard-focusable.
// ---------------------------------------------------------------------------

/** A glossary entry's keys — MetricTip takes one of these. */
export type MetricTerm =
  | "total_time"
  | "input"
  | "cached_input"
  | "cache_reuse"
  | "output"
  | "reasoning"
  | "cost"
  | "setup"
  | "compaction"
  | "connect"
  | "first_token"
  | "generation"
  | "stalls"
  | "tools"
  | "retry_wait"
  | "finish"
  | "http"
  | "guard_cut"
  | "prefix_reuse"
  | "retained"
  | "live_estimate";

/** One glossary entry: the term and its plain-language explanation. */
export interface MetricGloss {
  term: string;
  body: string;
}

/** Every metric explanation the Trace shows, from the design's §7 table. */
export const TRACE_GLOSSARY: Record<MetricTerm, MetricGloss> = {
  total_time: {
    term: "Total time",
    body: "Wall time for this call. Tool execution and local setup are identified separately where applicable.",
  },
  input: {
    term: "Input",
    body: "Tokens sent to the model, including instructions, history, and tool schemas.",
  },
  cached_input: {
    term: "Cached input",
    body: "Input tokens the provider says it served from cache.",
  },
  cache_reuse: {
    term: "Cache reuse",
    body: "Percentage of input reported as cached. “Not reported” is distinct from 0% when usage is absent.",
  },
  output: {
    term: "Output",
    body: "Tokens generated by the model, including reasoning tokens.",
  },
  reasoning: {
    term: "Reasoning",
    body: "The portion of output used for model reasoning; it is not added to Output twice.",
  },
  cost: {
    term: "Estimated cost",
    body: "Estimate from current model pricing and final token counts. It can differ from the provider bill and is unavailable without a price.",
  },
  setup: {
    term: "Setup",
    body: "Local time preparing the request: routing, token accounting, recall, and prompt building.",
  },
  compaction: {
    term: "Compaction",
    body: "Time spent shortening earlier context. Included in Setup, not extra time.",
  },
  connect: {
    term: "Connect",
    body: "Time from sending the request until response headers arrive: connection, upload, and server acknowledgement.",
  },
  first_token: {
    term: "First token",
    body: "Wait after response headers until the first streamed chunk.",
  },
  generation: {
    term: "Generation",
    body: "Time from the first chunk until the final response/usage chunk.",
  },
  stalls: {
    term: "Stalls",
    body: "Silences of more than one second inside Generation. Included in Generation, not extra time.",
  },
  tools: {
    term: "Tools",
    body: "Time running the tools requested by this response, after model streaming ends.",
  },
  retry_wait: {
    term: "Retry wait",
    body: "Delay before a retry attempt; never counted again as Connect.",
  },
  finish: {
    term: "Finish",
    body: "Why the model stopped, translated where known (Completed, Tool calls, Length limit).",
  },
  http: {
    term: "HTTP",
    body: "Provider HTTP response code, shown mainly for failures and diagnostics.",
  },
  guard_cut: {
    term: "Guard cut",
    body: "A client-side stream limit ended this call. The limit name and threshold live in Raw.",
  },
  prefix_reuse: {
    term: "Prefix reuse",
    body: "This measures identical request text. It does not prove the provider cached those tokens.",
  },
  retained: {
    term: "Retained metadata",
    body: "This call survived a restart without its prompt or response. Enable full-body logging for future calls.",
  },
  live_estimate: {
    term: "Live estimate",
    body: "Streaming counts so far; the provider's final usage replaces them when the call ends, labelled Final.",
  },
};

// ---------------------------------------------------------------------------
// Per-project preferences — local storage, the theme's storage pattern.
// ---------------------------------------------------------------------------

/** The persisted view state, a strict subset of what the route keeps. */
export interface TraceViewPrefs extends Partial<TraceFilters> {
  /** Whether the collapsible Insights panel is open. */
  insightsOpen?: boolean;
  /** The list column's width in px; null keeps the 420 px default. */
  listWidth?: number | null;
}

const PREF_KEY_PREFIX = "mnemo2.trace.";

/** Read the saved view state for one project; `null` when none stored. */
export function loadTracePrefs(projectId: string | null): TraceViewPrefs | null {
  if (projectId === null) return null;
  try {
    const raw = window.localStorage.getItem(PREF_KEY_PREFIX + projectId);
    if (raw === null) return null;
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return parsed as TraceViewPrefs;
  } catch {
    return null;
  }
}

/** Best-effort save; storage being unavailable never breaks the route. */
export function saveTracePrefs(projectId: string | null, prefs: TraceViewPrefs): void {
  if (projectId === null) return;
  try {
    window.localStorage.setItem(PREF_KEY_PREFIX + projectId, JSON.stringify(prefs));
  } catch {
    // Private-mode storage or a quota cap — the route works without saving.
  }
}

// ---------------------------------------------------------------------------
// Route entry — the filter hand-over `openTrace` plants.
// ---------------------------------------------------------------------------

/** The filter hand-over an entry point can carry into the Trace route. */
export interface TraceEntry {
  agent?: string;
  model?: string;
  status?: TraceListStatus;
  purpose?: TracePurpose;
  from_ms?: number;
  to_ms?: number;
  turn_id?: string;
  search?: string;
  search_scope?: TraceSearchScope;
  /** The call to open on arrival ("run_id:id"), for the compact tab's hand-over. */
  selected?: string;
}

/**
 * Translate an entry hand-over into filter state. A time range becomes the
 * custom preset; an agent, model, status, purpose or turn pins its filter;
 * nothing else moves.
 */
export function traceEntryFilters(entry: TraceEntry | null): Partial<TraceFilters> | null {
  if (entry === null) return null;
  const patch: Partial<TraceFilters> = {};
  if (entry.agent !== undefined) patch.agent = entry.agent;
  if (entry.model !== undefined) patch.model = entry.model;
  if (entry.status !== undefined) patch.status = entry.status;
  if (entry.purpose !== undefined) patch.purpose = entry.purpose;
  if (entry.search !== undefined) {
    patch.search = entry.search;
    patch.scope = entry.search_scope ?? "all";
  }
  if (entry.turn_id !== undefined) patch.turn_id = entry.turn_id;
  if (entry.selected !== undefined) patch.selected = entry.selected;
  if (entry.from_ms !== undefined || entry.to_ms !== undefined) {
    patch.time = "custom";
    patch.from_ms = entry.from_ms ?? null;
    patch.to_ms = entry.to_ms ?? null;
  }
  return Object.keys(patch).length > 0 ? patch : null;
}