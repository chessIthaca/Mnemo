// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useMemo, useRef, useState } from "react";
import { Activity, Ban, ChevronDown, ChevronRight, Eraser, GitCompareArrows, TriangleAlert } from "lucide-react";
import {
  listLlmRequests,
  getLlmRequest,
  clearLlmRequests,
  getTraceLogging,
  setTraceLogging,
  getSteeringStats,
  errMsg,
} from "../../lib/tauri";
import type { LlmRequestDetail, LlmRequestSummary, LlmUsage, SteeringStatsSnapshot } from "../../lib/types";
import { fmtDuration, fmtPct } from "../../lib/format";
import { cacheHitPct } from "../../lib/traceStats";
import { JsonView } from "../common/JsonView";
import { TraceStats } from "./TraceStats";

/**
 * Trace tab — a live inspector for every request sent to the OpenAI-compatible
 * `/chat/completions` endpoint. Polls `listLlmRequests()` while mounted (the
 * view only mounts when the tab is active). A resizable stats strip sits at
 * the top (vertical column charts, newest rightmost, Fill/Relative scale
 * toggle — see TraceStats), with a drag splitter between it and the log.
 * Below the splitter: a message wall of requests on the left (each row shows
 * model · provider); selecting one loads its full detail on the right:
 * usage/cache card, a smart view of the exact request JSON (collapsed
 * messages, expandable, with a Raw JSON toggle), and the raw response.
 *
 * Purpose: cache-hit optimization. Each row shows the cached/prompt ratio;
 * the detail shows exactly what was sent (so you can see what changed vs the
 * previous request and broke the provider's cache prefix).
 */

// ── Formatting helpers ──────────────────────────────────────────────────────

/** Local time as HH:MM:SS (from unix epoch ms). */
function fmtTime(ms: number): string {
  return new Date(ms).toLocaleTimeString();
}

/** Comma-grouped integer (e.g. 6000 → "6,000"). */
function fmtInt(n: number): string {
  return Math.round(n).toLocaleString("en-US");
}

/** A compact per-phase duration: "123ms", "1.2s", "1m 3s". */
function fmtPhaseMs(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  return fmtDuration(ms);
}

/**
 * The per-request phase-timing line: p = prep (local prompt build), c =
 * compact (auto-compaction call), b = backoff (retry sleeps before this
 * attempt), n = connect (record → headers: body build + upload + server
 * prefill/queue), w = wait (ttft: POST-send → first token — the real
 * first-token wait), r = reason (reasoning deltas), g = generate (answer +
 * the stalled byte-silence inside the generation window — the stalled share
 * is shown parenthetically as `s`, mirroring the dithered hatch inside the
 * TraceStats generate segment), t = tools (stream end → tool batch done).
 * Renders nothing until at least one phase has a value.
 */
function PhaseTimes({
  prep,
  compact,
  backoff,
  connect,
  ttft,
  reason,
  gen,
  stall,
  tools,
}: {
  prep: number | null;
  compact: number | null;
  backoff: number | null;
  connect: number | null;
  ttft: number | null;
  reason: number | null;
  gen: number | null;
  stall: number | null;
  tools: number | null;
}) {
  // `gen` (generation_ms) is inclusive (thinking + answer + stalled
  // silence); the reason share is broken out separately, so display g as
  // gen − reason. Stall is NOT subtracted — it is byte-silence inside the
  // generation window, so it counts as generate (the same convention as the
  // TraceStats chart, where it renders as the dithered hatch inside the
  // generate segment); the parenthetical keeps the exact stalled time
  // readable.
  const genShown = gen != null ? Math.max(0, gen - (reason ?? 0)) : null;
  const parts: string[] = [];
  if (prep != null) parts.push(`p ${fmtPhaseMs(prep)}`);
  if (compact != null) parts.push(`c ${fmtPhaseMs(compact)}`);
  if (backoff != null) parts.push(`b ${fmtPhaseMs(backoff)}`);
  if (connect != null) parts.push(`n ${fmtPhaseMs(connect)}`);
  if (ttft != null) parts.push(`w ${fmtPhaseMs(ttft)}`);
  if (reason != null) parts.push(`r ${fmtPhaseMs(reason)}`);
  if (genShown != null) {
    parts.push(
      stall != null && stall > 0
        ? `g ${fmtPhaseMs(genShown)} (s ${fmtPhaseMs(stall)})`
        : `g ${fmtPhaseMs(genShown)}`,
    );
  }
  if (tools != null) parts.push(`t ${fmtPhaseMs(tools)}`);
  if (parts.length === 0) return null;
  return (
    <span title="prep / compact / backoff / connect (record→headers) / wait (ttft = send→first token) / reason / generate (s = stalled share) / tools">
      {parts.join(" · ")}
    </span>
  );
}

/**
 * The detail-poll stop predicate (reviews N1 + L1/F1, 2026-08-18): stop
 * re-fetching once the record is terminal (`is_complete` — finish_reason or
 * error present) or absent (`null` — never recorded or ring-evicted; ids
 * are never reused and the ring only loses records, so a null can never
 * become non-null). A transient fetch ERROR is not a stop signal — the
 * caller's catch path keeps polling so IPC hiccups recover. Pure —
 * extracted so the vitest suite can pin the exact table.
 */
export function shouldStopPolling(d: LlmRequestDetail | null): boolean {
  return d === null || d.is_complete;
}

/**
 * The detail-fetch skip predicate (perf review L5, 2027-01-09): re-fetch the
 * full record (up to ~2 MiB) only when its mutation `version` changed since
 * the last fetch. `lastVersion` null = no detail fetched yet (or the
 * selection changed) — always fetch; `listVersion` null = the record is
 * absent from the list (never recorded / ring-evicted) — always fetch, so
 * the poll can observe the null and stop. Pure — extracted so the vitest
 * suite can pin the exact table.
 */
export function shouldSkipRefetch(
  lastVersion: number | null,
  listVersion: number | null,
): boolean {
  return lastVersion !== null && listVersion !== null && lastVersion === listVersion;
}

/**
 * The row-badge derivation (D1, 2027-01-10): `failed` when the record
 * carries an error or an HTTP failure status; `cancelled` when the consumer
 * dropped the stream mid-flight (a user interrupt/cancel/compact/clear) —
 * shown only when the row is not failed, so a genuine error that landed
 * just before the drop keeps its ERR badge. Pure — extracted so the vitest
 * suite can pin the exact table.
 */
export function rowBadges(r: Pick<LlmRequestSummary, "error" | "http_status" | "cancelled">): {
  failed: boolean;
  cancelled: boolean;
} {
  const failed = r.error != null || (r.http_status ?? 0) >= 400;
  return { failed, cancelled: r.cancelled && !failed };
}

/** The record's mutation version from the latest list poll (null = absent). */
function listVersionOf(requests: LlmRequestSummary[], id: number): number | null {
  const r = requests.find((x) => x.id === id);
  return r ? r.version : null;
}

/** The first line of a string, truncated for the row preview. */
function firstLine(s: string, max = 90): string {
  const line = s.split("\n")[0];
  return line.length > max ? `${line.slice(0, max)}…` : line;
}

/** A message's content as a plain string (string content or stringified parts). */
function contentString(content: unknown): string {
  if (typeof content === "string") return content;
  if (content == null) return "";
  return JSON.stringify(content);
}

// ── List-row pieces ─────────────────────────────────────────────────────────

/** The green "N% cached" badge (absent when the provider reported no usage). */
function CacheBadge({ usage }: { usage: LlmUsage | null }) {
  const pct = cacheHitPct(usage);
  if (pct === null) return null;
  return pct > 0 ? (
    <span className="rounded bg-emerald-500/15 px-1 py-px font-mono text-[0.65em] font-semibold text-emerald-400">
      {fmtPct(pct)}% cached
    </span>
  ) : (
    <span className="rounded bg-bg-tertiary px-1 py-px font-mono text-[0.65em] text-slate-500">
      0% cached
    </span>
  );
}

/** One request row in the left message wall. */
function RequestRow({
  r,
  selected,
  onClick,
}: {
  r: LlmRequestSummary;
  selected: boolean;
  onClick: () => void;
}) {
  const badges = rowBadges(r);
  return (
    <button
      onClick={onClick}
      className={`w-full border-l-2 px-2 py-1.5 text-left transition-colors ${
        selected
          ? "border-cyan-500 bg-cyan-500/10"
          : "border-transparent hover:bg-bg-tertiary"
      }`}
    >
      <div className="flex items-center justify-between gap-1">
        <span className="font-mono text-[0.68em] text-slate-400">{fmtTime(r.ts_ms)}</span>
        <span className="flex items-center gap-1">
          {badges.failed && (
            <span className="flex items-center gap-0.5 text-[0.65em] font-semibold text-red-400">
              <TriangleAlert className="h-3 w-3" /> ERR
            </span>
          )}
          {badges.cancelled && (
            <span className="flex items-center gap-0.5 text-[0.65em] font-semibold text-slate-400">
              <Ban className="h-3 w-3" /> CANCELLED
            </span>
          )}
          <CacheBadge usage={r.usage} />
        </span>
      </div>
      <div
        className="truncate text-[0.75em] text-slate-300"
        title={`${r.model} · ${r.provider}`}
      >
        {r.model} <span className="text-slate-500">· {r.provider}</span>
      </div>
      <div className="flex items-center gap-2 font-mono text-[0.65em] text-slate-500">
        {r.usage ? (
          <>
            {/* Prompt stays 0 until the authoritative usage lands, so an
                in-flight (streaming estimate) row shows "—" instead of a
                misleading literal "0 in". */}
            <span>{r.is_complete ? fmtInt(r.usage.prompt) : "—"} in</span>
            <span>{fmtInt(r.usage.completion)} out</span>
          </>
        ) : (
          <span>{r.http_status ? `HTTP ${r.http_status}` : "…"}</span>
        )}
        <PhaseTimes prep={r.prep_ms} compact={r.compact_ms} backoff={r.backoff_ms} connect={r.connect_ms} ttft={r.ttft_ms} reason={r.reasoning_ms} gen={r.generation_ms} stall={r.stall_ms} tools={r.tools_ms} />
      </div>
    </button>
  );
}

// ── Usage card ──────────────────────────────────────────────────────────────

/** The usage/cache card shown above the request body. */
function UsageCard({ usage, finishReason, streaming, ttft, reason, gen, connect, stall, tools, prep, compact, backoff, prefix }: {
  usage: LlmUsage | null;
  finishReason: string | null;
  /** True while the request is still streaming — the usage numbers are live
   * chars/4 estimates (mostly reasoning deltas) that the authoritative count
   * replaces at stream end. */
  streaming: boolean;
  ttft: number | null;
  /** Reasoning phase (first reasoning delta → first answer delta). */
  reason: number | null;
  gen: number | null;
  /** Connect phase (record created → POST response headers). */
  connect: number | null;
  /** Mid-stream stall phase (byte-silence gaps inside the generation window). */
  stall: number | null;
  /** Tools phase (stream end → tool batch done). */
  tools: number | null;
  /** Local prep phase (turn-loop iteration start → the POST). */
  prep: number | null;
  /** Auto-compaction phase (the summarization LLM call), when it fired. */
  compact: number | null;
  /** Retry-backoff sleep that preceded this attempt. */
  backoff: number | null;
  /** Cache-prefix info from compare mode (null = compare off). */
  prefix: { k: number; n: number; pct: number | null } | null;
}) {
  const pct = cacheHitPct(usage);
  // `gen` (generation_ms) is inclusive (thinking + answer + stalled
  // silence); display g as gen − reason. Stall is NOT subtracted — it is
  // byte-silence inside the generation window, so it counts as generate
  // (the same convention as the TraceStats chart, where it renders as the
  // dithered hatch inside the generate segment); the parenthetical keeps
  // the exact stalled time readable.
  const genShown = gen != null ? Math.max(0, gen - (reason ?? 0)) : null;
  return (
    <div className="rounded border border-border p-2">
      <div className="grid grid-cols-4 gap-2 text-center">
        <div>
          <div className="font-mono text-sm text-slate-200">{usage ? fmtInt(usage.prompt) : "—"}</div>
          <div className="text-[0.62em] uppercase tracking-wide text-slate-500">prompt</div>
        </div>
        <div>
          <div className="font-mono text-sm text-slate-200">{usage ? fmtInt(usage.completion) : "—"}</div>
          <div className="text-[0.62em] uppercase tracking-wide text-slate-500">completion</div>
        </div>
        <div>
          <div className="font-mono text-sm text-slate-200">{usage ? fmtInt(usage.reasoning) : "—"}</div>
          <div className="text-[0.62em] uppercase tracking-wide text-slate-500">reasoning</div>
        </div>
        <div>
          <div className="font-mono text-sm text-emerald-400">{usage ? fmtInt(usage.cached) : "—"}</div>
          <div className="text-[0.62em] uppercase tracking-wide text-slate-500">cached</div>
        </div>
      </div>
      {streaming && usage && (
        <div
          className="mt-1 text-center text-[0.62em] uppercase tracking-wide text-amber-400/80"
          title="Live chars/4 estimate while the stream runs (mostly reasoning deltas) — replaced by the provider's authoritative usage when the request finishes."
        >
          est. while streaming
        </div>
      )}
      {usage && pct !== null && (
        <>
          <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-bg-tertiary">
            <div
              className={`h-full rounded-full ${pct > 0 ? "bg-emerald-500" : "bg-slate-600"}`}
              style={{ width: `${pct}%` }}
            />
          </div>
          <div className="mt-1 flex items-center justify-between text-[0.65em] text-slate-500">
            <span>
              cache hit <span className={pct > 0 ? "font-semibold text-emerald-400" : ""}>{fmtPct(pct)}%</span>
            </span>
            <span className="font-mono">
              {[
                finishReason && `finish: ${finishReason}`,
                prep != null && `p ${fmtPhaseMs(prep)}`,
                compact != null && `c ${fmtPhaseMs(compact)}`,
                backoff != null && `b ${fmtPhaseMs(backoff)}`,
                connect != null && `n ${fmtPhaseMs(connect)}`,
                ttft != null && `w ${fmtPhaseMs(ttft)}`,
                reason != null && `r ${fmtPhaseMs(reason)}`,
                genShown != null &&
                  (stall != null && stall > 0
                    ? `g ${fmtPhaseMs(genShown)} (s ${fmtPhaseMs(stall)})`
                    : `g ${fmtPhaseMs(genShown)}`),
                tools != null && `t ${fmtPhaseMs(tools)}`,
              ]
                .filter(Boolean)
                .join(" · ")}
            </span>
          </div>
        </>
      )}
      {prefix && <PrefixLine k={prefix.k} n={prefix.n} pct={prefix.pct} />}
    </div>
  );
}

// ── REQUEST section ─────────────────────────────────────────────────────────

/** A message as it appears in the wire request JSON. */
interface TraceMessage {
  role?: string;
  name?: string;
  content?: unknown;
  tool_call_id?: string;
  tool_calls?: { id?: string; function?: { name?: string; arguments?: string } }[];
  [k: string]: unknown;
}

/** A tool definition as it appears in the wire request JSON. */
interface TraceTool {
  type?: string;
  function?: { name?: string; description?: string; parameters?: unknown };
}

const ROLE_STYLES: Record<string, string> = {
  system: "bg-slate-700 text-slate-200",
  user: "bg-blue-900/60 text-blue-300",
  assistant: "bg-cyan-900/60 text-cyan-300",
  tool: "bg-amber-900/60 text-amber-300",
};

/** One collapsed message row — expandable to the message's pretty JSON. */
function MessageRow({
  msg,
  index,
  expanded,
  onToggle,
  diff,
}: {
  msg: TraceMessage;
  index: number;
  expanded: boolean;
  onToggle: () => void;
  diff?: DiffStatus;
}) {
  const role = typeof msg.role === "string" ? msg.role : "?";
  const name = typeof msg.name === "string" ? msg.name : null;
  const toolCalls = Array.isArray(msg.tool_calls) ? msg.tool_calls : [];
  const content = contentString(msg.content);
  const preview =
    firstLine(content) ||
    (toolCalls.length > 0
      ? `tool_calls: ${toolCalls.map((tc) => tc?.function?.name ?? "?").join(", ")}`
      : "(empty)");
  return (
    <div className="rounded border border-border">
      <button
        onClick={onToggle}
        className="flex w-full items-center gap-2 px-2 py-1 text-left hover:bg-bg-tertiary"
      >
        <span className={`shrink-0 rounded px-1.5 py-0.5 font-mono text-[0.62em] font-semibold uppercase ${ROLE_STYLES[role] ?? "bg-bg-tertiary text-slate-400"}`}>
          {role}
        </span>
        {diff && <DiffBadge status={diff} />}
        {name && <span className="shrink-0 font-mono text-[0.68em] text-slate-400">{name}</span>}
        <span className="shrink-0 font-mono text-[0.62em] text-slate-500">{fmtInt(content.length)} ch</span>
        <span className="min-w-0 flex-1 truncate text-[0.7em] text-slate-300">{preview}</span>
        <span className="text-slate-500">#{index + 1}</span>
        {expanded ? (
          <ChevronDown className="h-3.5 w-3.5 shrink-0 text-slate-500" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 shrink-0 text-slate-500" />
        )}
      </button>
      {expanded && (
        <div className="border-t border-border">
          <JsonView data={msg} defaultDepth={1} />
        </div>
      )}
    </div>
  );
}

/** Top-level k-v chips (model / stream / max_completion_tokens / ...). */
function TopLevelChips({ req }: { req: LlmRequestDetail }) {
  const keys = [
    "model",
    "stream",
    "max_completion_tokens",
    "reasoning_effort",
    "tool_choice",
  ] as const;
  const chips = keys
    .map((k) => ({ k, v: req.request_json[k] }))
    .filter(({ v }) => v !== undefined && v !== null);
  if (chips.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-1">
      {chips.map(({ k, v }) => (
        <span
          key={k}
          className="rounded border border-border bg-bg-tertiary px-1.5 py-0.5 font-mono text-[0.65em] text-slate-400"
        >
          <span className="text-slate-500">{k}:</span>{" "}
          <span className="text-slate-200">
            {typeof v === "object" ? JSON.stringify(v) : String(v)}
          </span>
        </span>
      ))}
    </div>
  );
}

/** The REQUEST section: smart view by default, full pretty JSON via toggle. */
function RequestSection({
  req,
  compare,
}: {
  req: LlmRequestDetail;
  /** Per-message diff vs the previous request (null = compare off). */
  compare: { statuses: DiffStatus[]; removed: number } | null;
}) {
  const [rawJson, setRawJson] = useState(false);
  const [expandedMsgs, setExpandedMsgs] = useState<Set<number>>(new Set());
  const [toolsOpen, setToolsOpen] = useState(false);

  const messages: TraceMessage[] = Array.isArray(req.request_json.messages)
    ? (req.request_json.messages as TraceMessage[])
    : [];
  const tools: TraceTool[] = Array.isArray(req.request_json.tools)
    ? (req.request_json.tools as TraceTool[])
    : [];

  const toggleMsg = (i: number) =>
    setExpandedMsgs((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });

  return (
    <div className="rounded border border-border">
      <div className="flex items-center justify-between border-b border-border px-2 py-1">
        <span className="text-[0.68em] font-semibold uppercase tracking-wide text-slate-400">
          Request
        </span>
        <button
          onClick={() => setRawJson((v) => !v)}
          className={`rounded px-1.5 py-0.5 text-[0.65em] font-medium transition-colors ${
            rawJson
              ? "bg-cyan-500/20 text-cyan-300"
              : "bg-bg-tertiary text-slate-400 hover:text-slate-200"
          }`}
        >
          {rawJson ? "Smart view" : "Raw JSON"}
        </button>
      </div>

      {rawJson ? (
        <JsonView data={req.request_json} defaultDepth={2} />
      ) : (
        <div className="space-y-1.5 p-2">
          <TopLevelChips req={req} />

          {/* Message wall — collapsed rows, expandable to per-message JSON. */}
          <div className="space-y-1">
            {messages.map((msg, i) => (
              <MessageRow
                key={i}
                msg={msg}
                index={i}
                expanded={expandedMsgs.has(i)}
                onToggle={() => toggleMsg(i)}
                diff={compare ? compare.statuses[i] : undefined}
              />
            ))}
            {compare && compare.removed > 0 && (
              <div className="rounded border border-red-500/30 bg-red-950/20 px-2 py-1 text-[0.68em] text-red-300">
                <DiffBadge status="removed" /> {compare.removed} message
                {compare.removed === 1 ? "" : "s"} from the previous request dropped from
                this one.
              </div>
            )}
            {messages.length === 0 && (
              <div className="text-[0.7em] text-slate-500">(no messages in request)</div>
            )}
          </div>

          {/* Tools — collapsed to a count chip, expandable to name+description. */}
          {tools.length > 0 && (
            <div className="rounded border border-border">
              <button
                onClick={() => setToolsOpen((v) => !v)}
                className="flex w-full items-center gap-1.5 px-2 py-1 text-left hover:bg-bg-tertiary"
              >
                {toolsOpen ? (
                  <ChevronDown className="h-3.5 w-3.5 text-slate-500" />
                ) : (
                  <ChevronRight className="h-3.5 w-3.5 text-slate-500" />
                )}
                <span className="text-[0.7em] text-slate-300">
                  {tools.length} tool{tools.length === 1 ? "" : "s"}
                </span>
              </button>
              {toolsOpen && (
                <ul className="space-y-1 border-t border-border p-2">
                  {tools.map((t, i) => (
                    <li key={i} className="text-[0.7em]">
                      <span className="font-mono text-cyan-300">
                        {t?.function?.name ?? `tool #${i + 1}`}
                      </span>
                      <span className="text-slate-500"> — </span>
                      <span className="text-slate-400">{t?.function?.description ?? "(no description)"}</span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ── RESPONSE section ────────────────────────────────────────────────────────

/** Raw response in a <pre>, collapsed to the first ~2 KB with an Expand toggle. */
function ResponseSection({ detail }: { detail: LlmRequestDetail }) {
  const [expanded, setExpanded] = useState(false);
  const raw = detail.response_raw ?? "";
  const COLLAPSE_AT = 2048;
  const truncatedLocal = raw.length > COLLAPSE_AT;
  const shown = expanded ? raw : raw.slice(0, COLLAPSE_AT);

  return (
    <div className="rounded border border-border">
      <div className="flex items-center justify-between border-b border-border px-2 py-1">
        <span className="text-[0.68em] font-semibold uppercase tracking-wide text-slate-400">
          Response
        </span>
        <span className="font-mono text-[0.65em] text-slate-500">
          {detail.http_status ? `HTTP ${detail.http_status}` : "…"}
          {detail.finish_reason ? ` · ${detail.finish_reason}` : ""}
          {detail.guard_cut ? (
            <span
              className="text-amber-400"
              title={`stream guard cut on ${detail.guard_cut.boundary} (byte ${detail.guard_cut.byte_idx}, ${detail.guard_cut.chars_before} chars in)`}
            >
              {" · guard cut"}
            </span>
          ) : null}
          {detail.error ? " · error" : ""}
        </span>
      </div>
      {raw.length === 0 ? (
        <div className="p-2 text-[0.7em] text-slate-500">
          {detail.response_evicted
            ? "Raw response evicted by the trace memory budget (row metadata survives). Raise the budget in Settings → Advanced to keep more."
            : detail.error
              ? "No response body (request failed before any bytes arrived)."
              : "Waiting for response bytes…"}
        </div>
      ) : (
        <>
          <pre className="max-h-80 overflow-auto p-2 font-mono text-[0.68em] leading-relaxed text-slate-300">
            {shown}
            {!expanded && truncatedLocal && (
              <span className="text-slate-500">{`\n… (${fmtInt(raw.length - COLLAPSE_AT)} more bytes)`}</span>
            )}
          </pre>
          <div className="flex items-center justify-between border-t border-border px-2 py-1">
            {truncatedLocal && (
              <button
                onClick={() => setExpanded((v) => !v)}
                className="rounded bg-bg-tertiary px-1.5 py-0.5 text-[0.65em] font-medium text-slate-400 hover:text-slate-200"
              >
                {expanded ? "Collapse" : `Expand (${fmtInt(raw.length)} bytes)`}
              </button>
            )}
            {detail.response_truncated && (
              <span className="text-[0.62em] text-amber-400/90">
                backend capture truncated at 2 MiB — this is a prefix of the real response
              </span>
            )}
          </div>
        </>
      )}
    </div>
  );
}

// ── DELIVERED ARGS section (backlog e8b39d72 H1) ────────────────────────────

/** The tool calls AS DELIVERED by the model — the generation-side tap. When a
 *  future incident lands a corrupted edit, compare this against the
 *  executed/normalized args to separate model-emission decay from
 *  harness-layer mutation. Budget-exempt: survives response_raw eviction. */
function DeliveredToolCallsSection({ detail }: { detail: LlmRequestDetail }) {
  const calls = detail.raw_tool_calls ?? [];
  if (calls.length === 0) return null;
  return (
    <div className="rounded border border-border">
      <div className="flex items-center justify-between border-b border-border px-2 py-1">
        <span className="text-[0.68em] font-semibold uppercase tracking-wide text-slate-400">
          Delivered tool calls
        </span>
        <span className="font-mono text-[0.65em] text-slate-500">
          {calls.length} as generated, pre-normalization
        </span>
      </div>
      {calls.map((c, i) => (
        <div key={c.id || i} className="border-b border-border p-2 last:border-b-0">
          <div className="mb-1 font-mono text-[0.68em] text-slate-400">
            {c.name}
            {c.truncated && (
              <span className="ml-2 text-amber-400/90">(args truncated at 4 KiB)</span>
            )}
          </div>
          <pre className="max-h-60 overflow-auto font-mono text-[0.66em] leading-relaxed text-slate-300">
            {c.arguments}
          </pre>
        </div>
      ))}
    </div>
  );
}

// ── Compare-with-previous mode ──────────────────────────────────────────────

/**
 * How one message in the current request differs from the message at the same
 * index in the previous request. The cacheable prefix is the run of leading
 * `unchanged` messages — the first `changed`/`added` message is where the
 * provider's cache prefix breaks.
 */
type DiffStatus = "unchanged" | "changed" | "added" | "removed";

const DIFF_STYLES: Record<DiffStatus, string> = {
  unchanged: "bg-slate-700/60 text-slate-400",
  changed: "bg-amber-500/20 text-amber-300",
  added: "bg-emerald-500/20 text-emerald-300",
  removed: "bg-red-500/20 text-red-300",
};

const DIFF_LABELS: Record<DiffStatus, string> = {
  unchanged: "unchanged",
  changed: "changed",
  added: "added",
  removed: "removed",
};

/** Serialized-content equality of two messages (same as the provider sees). */
function messagesEqual(a: TraceMessage, b: TraceMessage): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/**
 * Compare the current request's messages against the previous request's
 * messages, index by index:
 * - identical content → "unchanged" (grey)
 * - different content → "changed" (amber)
 * - new index (current has more) → "added" (green)
 * - messages in the previous request beyond the current length → counted in
 *   `removed` (red note in the UI)
 *
 * Returns a status per current-message index plus the number of leading
 * identical messages (k) — the cacheable-prefix length.
 */
function compareMessages(
  current: TraceMessage[],
  previous: TraceMessage[]
): { statuses: DiffStatus[]; prefixK: number; removed: number } {
  const statuses: DiffStatus[] = current.map((msg, i) => {
    if (i < previous.length) {
      return messagesEqual(msg, previous[i]) ? "unchanged" : "changed";
    }
    return "added";
  });
  const removed = Math.max(0, previous.length - current.length);
  let prefixK = 0;
  for (const s of statuses) {
    if (s === "unchanged") prefixK += 1;
    else break;
  }
  return { statuses, prefixK, removed };
}

/** Small badge rendering a diff status on a message row. */
function DiffBadge({ status }: { status: DiffStatus }) {
  return (
    <span className={`shrink-0 rounded px-1 py-px text-[0.6em] font-semibold uppercase ${DIFF_STYLES[status]}`}>
      {DIFF_LABELS[status]}
    </span>
  );
}

/** The "first k of n messages identical" cache-prefix line in the usage card. */
function PrefixLine({ k, n, pct }: { k: number; n: number; pct: number | null }) {
  if (n === 0) return null;
  if (k === 0) {
    return (
      <div className="mt-1 text-[0.65em] text-red-300/90">
        No leading messages match the previous request — even message 1 breaks the
        cache prefix.{" "}
        {pct !== null && <span className="text-slate-400">Measured: {fmtPct(pct)}% cached.</span>}
      </div>
    );
  }
  if (k === n) {
    return (
      <div className="mt-1 text-[0.65em] text-emerald-400">
        All {n} messages identical to the previous request — the full prompt is
        cacheable ({pct !== null ? `${fmtPct(pct)}% measured` : "usage not reported"}).
      </div>
    );
  }
  return (
    <div className="mt-1 text-[0.65em] text-amber-300/90">
      First {k} of {n} messages identical to the previous request — the cacheable
      prefix ends at message {k} (message {k + 1} breaks it).{" "}
      {pct !== null && <span className="text-slate-400">Measured: {fmtPct(pct)}% cached.</span>}
    </div>
  );
}

// ── Main view ───────────────────────────────────────────────────────────────

const POLL_MS = 1500;

/** Min height (px) for the top stats section. */
const STATS_MIN = 140;
/** localStorage key persisting the stats section's height across sessions. */
const STATS_HEIGHT_KEY = "tracestats.height";
/** Default stats-section height (px) — room for two stacked column charts. */
const STATS_DEFAULT = 360;

/**
 * Clamp a stats-section height (px) to `[STATS_MIN, total * 0.7]`, rounded.
 * `total` is the Trace tab's rendered height. Pure — extracted so the vitest
 * suite can pin the clamp table for the drag-to-resize handle.
 */
export function clampStatsHeight(px: number, total: number): number {
  return Math.round(Math.max(STATS_MIN, Math.min(total * 0.7, px)));
}

/** Read the persisted stats-section height (default when absent/corrupt). */
function storedStatsHeight(): number {
  const v = Number(localStorage.getItem(STATS_HEIGHT_KEY));
  return Number.isFinite(v) && v >= STATS_MIN ? v : STATS_DEFAULT;
}

export function LlmTraceView() {
  const [requests, setRequests] = useState<LlmRequestSummary[]>([]);
  // Latest list poll, as a ref — the detail/compare polls read each record's
  // mutation `version` from it to skip re-fetching unchanged details (perf
  // review L5, 2027-01-09) without re-running their effects on every list
  // tick.
  const requestsRef = useRef<LlmRequestSummary[]>([]);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [detail, setDetail] = useState<LlmRequestDetail | null>(null);
  const [prevDetail, setPrevDetail] = useState<LlmRequestDetail | null>(null);
  const [compareOn, setCompareOn] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // "Log to file" checkbox — mirrors the backend toggle so it persists across
  // tab switches (the view unmounts when the tab is inactive).
  const [logToFile, setLogToFile] = useState(false);
  // Steering-effectiveness counters (advisory instrumentation; see
  // steering_stats.rs). Refreshed with the same 1.5s poll cadence as the
  // request list; a flat percentage when anything fired, absent otherwise.
  const [steering, setSteering] = useState<SteeringStatsSnapshot | null>(null);
  // Top stats-section height, drag-to-resize + persisted (mirrors the Graph
  // tab's bottom pane).
  const [statsHeight, setStatsHeight] = useState<number>(storedStatsHeight);
  // Root flex column — measures the total height for the stats clamp.
  const containerRef = useRef<HTMLDivElement | null>(null);
  // Tear down an in-flight stats drag if the tab unmounts mid-drag.
  const statsDragCleanup = useRef<(() => void) | null>(null);
  useEffect(() => {
    return () => {
      statsDragCleanup.current?.();
      statsDragCleanup.current = null;
    };
  }, []);
  // Re-clamp the persisted height against the CURRENT tab height on mount
  // (mirrors GraphView review N2): a height saved on a large window would
  // otherwise render unclamped on a smaller one until the user drags.
  useEffect(() => {
    const total = containerRef.current?.getBoundingClientRect().height;
    if (total) setStatsHeight((h) => clampStatsHeight(h, total));
  }, []);

  // Fetch the initial file-logging state on mount.
  useEffect(() => {
    let cancelled = false;
    getTraceLogging()
      .then((on) => {
        if (!cancelled) setLogToFile(on);
      })
      .catch((e) => {
        if (!cancelled) setError(errMsg(e));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Poll the request list every 1.5s while mounted (the view only mounts when
  // the tab is active). Selection is kept stable by id — when the selected
  // request is evicted from the ring buffer, fall back to the newest.
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const list = await listLlmRequests();
        if (cancelled) return;
        setRequests(list);
        // Feed the ref the detail/compare polls read for the version skip —
        // kept beside the state so both are always in lockstep.
        requestsRef.current = list;
        setError(null);
        setSelectedId((cur) => {
          if (cur !== null && list.some((r) => r.id === cur)) return cur;
          return list.length > 0 ? list[list.length - 1].id : null;
        });
      } catch (e) {
        if (!cancelled) setError(errMsg(e));
      }
    };
    void tick();
    const timer = setInterval(tick, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  // Fetch the steering counters on the same cadence — `get_steering_stats` is
  // one mutex snapshot (no I/O), so 1.5s polling is effectively free.
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const snap = await getSteeringStats();
        if (!cancelled) setSteering(snap);
      } catch {
        // Advisory metrics — never surface a poll failure as a banner error.
      }
    };
    void tick();
    const timer = setInterval(tick, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  // Fetch the selected detail on selection change AND on the same 1.5s cadence
  // — the raw response keeps growing while a request is streaming, so the
  // detail re-fetches live until the request finishes. The poll stops when
  // the record is terminal (`is_complete` — finish_reason or error present)
  // or absent (`null` — never recorded or ring-evicted; ids are never reused
  // and the ring only loses records, so a null can never become non-null):
  // re-fetching either only re-clones up to ~2 MiB per tick forever (review
  // N1, 2026-06-14 + review L1, 2026-08-18). Between fetches, the version
  // skip (perf review L5, 2027-01-09) consults the cheap list poll's mutation
  // counter and skips the full-record fetch while the record is unchanged —
  // an idle-but-incomplete record (TTFT window, stalled stream) no longer
  // re-clones its body every tick. A re-select restarts the effect;
  // a transient IPC error keeps polling (the catch path).
  useEffect(() => {
    if (selectedId === null) {
      setDetail(null);
      return;
    }
    let cancelled = false;
    // Clear the previous request's detail immediately — never show a stale
    // request while the newly selected one loads.
    setDetail(null);
    // Version of the detail currently held (null until the first fetch
    // lands) — effect-scoped, so a re-select always re-fetches.
    let lastVersion: number | null = null;
    let timer: ReturnType<typeof setInterval> | null = null;
    const stopPolling = () => {
      if (timer !== null) {
        clearInterval(timer);
        timer = null;
      }
    };
    const load = async () => {
      // Version skip (perf review L5, 2027-01-09): the list poll already
      // carries this record's mutation version; while it is unchanged since
      // the last fetch, skip re-fetching the full record — the detail can be
      // ~2 MiB of JSON per tick while a request streams.
      const listVersion = listVersionOf(requestsRef.current, selectedId);
      if (shouldSkipRefetch(lastVersion, listVersion)) return;
      try {
        const d = await getLlmRequest(selectedId);
        if (!cancelled) {
          setDetail(d);
          lastVersion = d === null ? null : d.version;
          if (shouldStopPolling(d)) stopPolling();
        }
      } catch (e) {
        if (!cancelled) setError(errMsg(e));
      }
    };
    void load();
    timer = setInterval(load, POLL_MS);
    return () => {
      cancelled = true;
      stopPolling();
    };
  }, [selectedId]);

  // Fetch the previous request (id-1) for compare mode — same cadence, so the
  // comparison updates live as both records fill in. A null result is expected
  // when id-1 was never recorded (first request) or was evicted from the ring.
  useEffect(() => {
    if (!compareOn || selectedId === null) {
      setPrevDetail(null);
      return;
    }
    // Clear the previous comparison immediately — never pair the new request
    // with the previous selection's "previous" while its own id-1 loads.
    setPrevDetail(null);
    let cancelled = false;
    // Version of the prev detail currently held (null until the first fetch
    // lands) — effect-scoped, so re-toggling compare always re-fetches.
    let lastVersion: number | null = null;
    let timer: ReturnType<typeof setInterval> | null = null;
    const stopPolling = () => {
      if (timer !== null) {
        clearInterval(timer);
        timer = null;
      }
    };
    const load = async () => {
      // Same version skip as the detail poll (perf review L5, 2027-01-09) —
      // the previous record re-fetches only when its version changed.
      const listVersion = listVersionOf(requestsRef.current, selectedId - 1);
      if (shouldSkipRefetch(lastVersion, listVersion)) return;
      try {
        const p = await getLlmRequest(selectedId - 1);
        if (!cancelled) {
          setPrevDetail(p);
          lastVersion = p === null ? null : p.version;
          // Same stop predicate as the detail poll: a null (never recorded /
          // ring-evicted — expected here) can never become non-null.
          if (shouldStopPolling(p)) stopPolling();
        }
      } catch (e) {
        if (!cancelled) setError(errMsg(e));
      }
    };
    void load();
    timer = setInterval(load, POLL_MS);
    return () => {
      cancelled = true;
      stopPolling();
    };
  }, [compareOn, selectedId]);

  // The per-message diff + cache-prefix info (null = compare off / no prev).
  const compare = useMemo(() => {
    if (!compareOn || !detail || !prevDetail) return null;
    const current: TraceMessage[] = Array.isArray(detail.request_json.messages)
      ? (detail.request_json.messages as TraceMessage[])
      : [];
    const previous: TraceMessage[] = Array.isArray(prevDetail.request_json.messages)
      ? (prevDetail.request_json.messages as TraceMessage[])
      : [];
    const { statuses, prefixK, removed } = compareMessages(current, previous);
    return {
      statuses,
      removed,
      prefixK,
      n: current.length,
      pct: cacheHitPct(detail.usage),
    };
  }, [compareOn, detail, prevDetail]);

  const clear = async () => {
    try {
      await clearLlmRequests();
      setRequests([]);
      // Keep the version-skip ref in lockstep with the state (the list poll
      // pairs its update the same way) — no reader may see the pre-clear
      // list after the records are gone.
      requestsRef.current = [];
      setSelectedId(null);
      setDetail(null);
      setError(null);
    } catch (e) {
      setError(errMsg(e));
    }
  };

  // Toggle file logging; on failure, revert the checkbox and surface the error.
  const toggleLogToFile = async (enabled: boolean) => {
    setLogToFile(enabled); // optimistic
    try {
      await setTraceLogging(enabled);
      setError(null);
    } catch (e) {
      setLogToFile(!enabled);
      setError(errMsg(e));
    }
  };

  /** Stats splitter drag: dragging DOWN grows the section (it sits at the
   *  top). The clamped height is committed to localStorage on drag END
   *  only — no synchronous write per pointermove (mirrors the Graph tab's
   *  bottom-pane splitter). */
  const onStatsSplitterDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const startY = e.clientY;
    const startHeight = statsHeight;
    const total = containerRef.current?.getBoundingClientRect().height ?? 600;
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);

    const onMove = (ev: PointerEvent) => {
      setStatsHeight(clampStatsHeight(startHeight + (ev.clientY - startY), total));
    };
    const endDrag = (ev: PointerEvent) => {
      try {
        handle.releasePointerCapture(ev.pointerId);
      } catch {
        // Capture may already be released (pointercancel) — ignore.
      }
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", endDrag);
      window.removeEventListener("pointercancel", endDrag);
      statsDragCleanup.current = null;
      // Persist the final height (re-clamped against the same total).
      const finalHeight = clampStatsHeight(startHeight + (ev.clientY - startY), total);
      setStatsHeight(finalHeight);
      localStorage.setItem(STATS_HEIGHT_KEY, String(finalHeight));
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", endDrag);
    // pointercancel fires when the OS interrupts the pointer (touch gesture,
    // system event) instead of pointerup — route it to the same cleanup so
    // the listeners don't leak.
    window.addEventListener("pointercancel", endDrag);
    statsDragCleanup.current = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", endDrag);
      window.removeEventListener("pointercancel", endDrag);
    };
  };

  // Newest first for display (the backend returns oldest-first).
  const rows = requests.slice().reverse();

  return (
    <div ref={containerRef} className="flex h-full min-h-0 flex-col">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-border px-2 py-1.5">
        <div className="flex items-center gap-1.5">
          <Activity className="h-3.5 w-3.5 text-cyan-400" />
          <span className="text-xs font-semibold text-slate-200">Trace</span>
          <span className="font-mono text-[0.62em] text-slate-500">
            {requests.length === 0 ? "" : `${requests.length} request${requests.length === 1 ? "" : "s"}`}
          </span>
          {steering && steering.fired > 0 && (
            <span
              className="font-mono text-[0.62em] text-slate-500"
              title={steering.by_marker
                .map(
                  (m) =>
                    `${m.marker}: ${m.fired} fired, ${m.switched} switched${m.gated ? `, ${m.gated} gated` : ""}`,
                )
                .join("\n")}
            >
              · steering {steering.switched}/{steering.fired} (
              {Math.round((steering.switched / steering.fired) * 100)}%)
            </span>
          )}
          {steering &&
            (steering.file_tool_mutations > 0 || steering.shell_mutations > 0) && (
              <span
                className="font-mono text-[0.62em] text-slate-500"
                title="File mutations this session by vehicle: file tools (file_edit/file_write/file_append, successful calls) vs shell commands matching a mutation pattern (Set-Content, Out-File, Add-Content, >>, sed -i, tee, python invoked with something to run; null-sink redirects excluded). Heuristic match — may over-count. The file-tools-first policy prefers the former."
              >
                · mutations file-tools {steering.file_tool_mutations} / shell{" "}
                {steering.shell_mutations}
              </span>
            )}
        </div>
        <div className="flex items-center gap-2">
          <label
            className="flex cursor-pointer items-center gap-1.5 text-[0.65em] text-slate-400 hover:text-slate-300"
            title="Append every captured request to .coding/logs/traces.jsonl (one JSON line per request, updated as it streams). Off by default — the file holds full request/response bodies in plaintext."
          >
            <input
              type="checkbox"
              checked={logToFile}
              onChange={(e) => void toggleLogToFile(e.target.checked)}
              className="h-3 w-3 accent-cyan-500"
            />
            Log to file
          </label>
          <button
            onClick={clear}
            disabled={requests.length === 0}
            className="flex items-center gap-1 rounded bg-bg-tertiary px-1.5 py-0.5 text-[0.65em] font-medium text-slate-400 transition-colors hover:text-slate-200 disabled:opacity-40"
          >
            <Eraser className="h-3 w-3" /> Clear
          </button>
        </div>
      </div>

      {error && (
        <div className="border-b border-red-600/40 bg-red-950/20 px-2 py-1 text-[0.7em] text-red-400">
          {error}
        </div>
      )}

      {/* Top: live stats graphs derived from the same polled rows, resized
          via the drag splitter between graphs and log (height persisted).
          Hidden when the ring is empty, mirroring TraceStats's
          null-when-empty. The splitter's border-y provides the separation;
          the strip paints the chrome-band background (bg-bg-secondary) —
          the lighter seam the horizontal bars always showed, which the
          vertical handle now matches instead of the other way around. */}
      {requests.length > 0 && (
        <>
          <TraceStats rows={requests} height={statsHeight} />
          <div
            onPointerDown={onStatsSplitterDown}
            role="separator"
            aria-orientation="horizontal"
            aria-label="Resize stats area"
            title="Drag to resize the stats area"
            className="group flex h-1.5 shrink-0 cursor-row-resize items-center justify-center border-y border-border bg-bg-secondary transition-colors hover:bg-cyan-500/20"
          >
            <div className="h-0.5 w-8 rounded-full bg-slate-500 group-hover:bg-slate-400" />
          </div>
        </>
      )}

      <div className="flex min-h-0 flex-1">
        {/* Left: message wall */}
        <div className="w-[250px] shrink-0 overflow-y-auto border-r border-border">
          {rows.length === 0 ? (
            <div className="p-3 text-center">
              <p className="text-[0.75em] text-slate-400">No requests recorded yet.</p>
              <p className="mt-1 text-[0.68em] leading-relaxed text-slate-500">
                Send a prompt and every /chat/completions request will appear here — request
                JSON + raw response — for cache-hit inspection.
              </p>
            </div>
          ) : (
            rows.map((r) => (
              <RequestRow
                key={r.id}
                r={r}
                selected={r.id === selectedId}
                onClick={() => setSelectedId(r.id)}
              />
            ))
          )}
        </div>

        {/* Right: detail */}
        <div className="min-w-0 flex-1 space-y-2 overflow-y-auto p-2">
          {detail ? (
            <>
              {/* Compare-with-previous toggle */}
              <div className="flex items-center justify-between">
                <button
                  onClick={() => setCompareOn((v) => !v)}
                  className={`flex items-center gap-1.5 rounded px-1.5 py-0.5 text-[0.68em] font-medium transition-colors ${
                    compareOn
                      ? "bg-cyan-500/20 text-cyan-300"
                      : "bg-bg-tertiary text-slate-400 hover:text-slate-200"
                  }`}
                >
                  <GitCompareArrows className="h-3 w-3" />
                  Compare with previous
                </button>
                {compareOn && !prevDetail && (
                  <span className="text-[0.65em] text-slate-500">
                    no previous request in the log (first request or evicted)
                  </span>
                )}
              </div>
              <UsageCard
                usage={detail.usage}
                finishReason={detail.finish_reason}
                streaming={!detail.is_complete}
                ttft={detail.ttft_ms}
                reason={detail.reasoning_ms}
                gen={detail.generation_ms}
                connect={detail.connect_ms}
                stall={detail.stall_ms}
                tools={detail.tools_ms}
                prep={detail.prep_ms}
                compact={detail.compact_ms}
                backoff={detail.backoff_ms}
                prefix={
                  compare ? { k: compare.prefixK, n: compare.n, pct: compare.pct } : null
                }
              />
              <RequestSection req={detail} compare={compare} />
              <ResponseSection detail={detail} />
        <DeliveredToolCallsSection detail={detail} />
            </>
          ) : (
            <div className="flex h-full items-center justify-center text-[0.75em] text-slate-500">
              {requests.length === 0
                ? "Nothing to inspect yet."
                : "Select a request to inspect it."}
            </div>
          )}
        </div>
      </div>

    </div>
  );
}
