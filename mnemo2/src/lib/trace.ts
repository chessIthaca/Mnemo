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

/** One row of the request list: the record without its big payloads. */
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
}

/** One poll of the log: the rows plus the ring's size. */
export interface TracePage {
  requests: TraceRow[];
  /** How many records the ring keeps before evicting the oldest. */
  capacity: number;
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

/** The IPC surface the Trace tab needs. */
export interface TraceTransport {
  /** The recorded requests, oldest first. */
  list(): Promise<TracePage>;
  /** One request in full, or `null` once it has been evicted. */
  detail(id: number): Promise<TraceDetail | null>;
  /** Drop every recorded request. */
  clear(): Promise<void>;
}

/** The dev-only preview's stand-in transport; never set in production. */
let previewSeed: TraceTransport | null = null;

/**
 * Point the Trace tab at fixture data for the dev-only preview page
 * (`npm run check:no-mock` keeps this out of the production entry).
 */
export function seedTracePreview(transport: TraceTransport | null): void {
  previewSeed = transport;
}

/** Build the Trace tab's transport, or `null` outside the desktop app. */
export function createTraceTransport(): TraceTransport | null {
  if (previewSeed) return previewSeed;
  if (!isTauriRuntime()) return null;
  return {
    list: () => invoke<TracePage>("trace_list"),
    detail: (id: number) => invoke<TraceDetail | null>("trace_detail", { id }),
    clear: () => invoke<void>("trace_clear"),
  };
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
 * What the row's status says: a refusal keeps its HTTP status, a cancelled
 * turn reads as cancelled, an in-flight one as streaming, and a clean turn
 * shows its finish reason.
 */
export function traceStatus(row: TraceRow): TraceStatus {
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

/** How many rows a fresh poll adds, for the list's "new" hint. */
export function newestId(page: TracePage): number | null {
  return page.requests.length === 0
    ? null
    : page.requests[page.requests.length - 1].id;
}
