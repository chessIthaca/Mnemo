// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { fmtDuration, fmtPct } from "./format";
import type { LlmRequestSummary, LlmUsage } from "./types";

/**
 * Pure data derivation for the Trace tab's stats charts. All series are
 * built from the same `LlmRequestSummary` list the trace view already polls
 * (oldest-first from the backend); the functions here only reshape it, so
 * the charts re-render live as the ring buffer fills in.
 */

/** One row of per-phase timing (ms) for the phase-time chart. */
export interface PhaseSeriesRow {
  id: number;
  /** Local HH:MM:SS of the request. */
  label: string;
  /** Local prompt-prep time EXCLUDING the compact window (prep_ms −
   *  compact_ms, clamped ≥ 0) so the stacked segments are disjoint and
   *  totalMs is honest wall time. The raw inclusive prep_ms stays on the
   *  wire/detail views. */
  prepMs: number;
  compactMs: number;
  /** Retry-backoff sleep that preceded this attempt (the 1s/2s sleeps
   *  between failed attempts) — waiting, not local work. */
  backoffMs: number;
  connectMs: number;
  waitMs: number;
  genMs: number;
  /** Reasoning time (first reasoning delta → first answer delta); a subset
   *  of genMs. */
  reasoningMs: number;
  /** Mid-stream stall time (byte-silence gaps > 2s inside the generation
   *  window); a subset of genMs — rendered as the dithered hatch INSIDE the
   *  generate segment (non-additive, like `cached` inside `prompt`), never
   *  as its own stacked slice. */
  stallMs: number;
  toolsMs: number;
  totalMs: number;
}

/** One row of token counts for the token chart. */
export interface TokenSeriesRow {
  id: number;
  label: string;
  prompt: number;
  cached: number;
  completion: number;
  reasoning: number;
}

/** Session-level aggregates for the stats summary strip. */
export interface TraceStatsSummary {
  count: number;
  totalPrepMs: number;
  totalCompactMs: number;
  totalBackoffMs: number;
  totalConnectMs: number;
  totalWaitMs: number;
  totalGenMs: number;
  totalStallMs: number;
  totalToolsMs: number;
  totalPrompt: number;
  totalCached: number;
  totalCompletion: number;
  /** Completion tokens per generation second (1 decimal); null when no gen time. */
  avgTokPerSec: number | null;
  /** Mean cache-hit % over rows with a measurable hit rate; null when none. */
  avgCacheHitPct: number | null;
}

/** Local HH:MM:SS label for a request row. */
function fmtLabel(tsMs: number): string {
  return new Date(tsMs).toLocaleTimeString();
}

/**
 * Cache-hit percentage for a usage record (null when unmeasurable). Same
 * formula the trace list rows use, rounded to at most 1 decimal (rendered
 * via {@link fmtPct}); clamped to 100 so a provider that reports
 * `cached > prompt` never renders an over-100% bar.
 */
export function cacheHitPct(usage: LlmUsage | null): number | null {
  if (!usage || usage.prompt === 0) return null;
  return Math.min(100, Math.round((usage.cached / usage.prompt) * 1000) / 10);
}

/**
 * How the column charts scale their bar heights: "fill" normalizes every
 * column to the full chart height (proportions within one request — the
 * pre-vertical layout's behavior); "relative" scales heights by the series
 * max so columns are comparable across requests (a 2-minute request's
 * column is 4× taller than a 30-second one).
 */
export type ScaleMode = "fill" | "relative";

/**
 * Column height as a percentage of the chart-body height for one series
 * value. "fill" maps any non-zero total to 100; "relative" maps it to
 * `total / max * 100` (0 when the series has no measurable max). Pure.
 */
export function columnHeightPct(total: number, max: number, mode: ScaleMode): number {
  if (mode === "fill") return total > 0 ? 100 : 0;
  return max > 0 ? (total / max) * 100 : 0;
}

/**
 * The most recent `max` rows in oldest-first order. The backend returns
 * oldest-first and charts want a left-to-right timeline, so the slice keeps
 * the input order (never reverse).
 */
export function latestRows(rows: LlmRequestSummary[], max: number): LlmRequestSummary[] {
  if (rows.length <= max) return rows.slice();
  return rows.slice(rows.length - max);
}

/** Per-request phase timings (prep / compact / backoff / connect / wait /
 *  reason / generate / tools) for the chart. */
export function phaseSeries(rows: LlmRequestSummary[], max = 24): PhaseSeriesRow[] {
  return latestRows(rows, max).map((r) => {
    const compactMs = r.compact_ms ?? 0;
    // prep_ms deliberately INCLUDES compact_ms (the compaction call runs
    // inside the prep window) — subtract it here so the stacked segments
    // are disjoint and totalMs is honest wall time.
    const prepMs = Math.max(0, (r.prep_ms ?? 0) - compactMs);
    const backoffMs = r.backoff_ms ?? 0;
    const connectMs = r.connect_ms ?? 0;
    const waitMs = r.ttft_ms ?? 0;
    const genMs = r.generation_ms ?? 0;
    const reasoningMs = r.reasoning_ms ?? 0;
    const stallMs = r.stall_ms ?? 0;
    // generation_ms INCLUDES the reasoning window — subtract it so the
    // stacked segments are disjoint (mirrors the prep−compact pattern).
    // Stall is NOT subtracted: it is byte-silence inside the generation
    // window, so it counts as generate and renders as the dithered hatch
    // overlay on the generate segment.
    const toolsMs = r.tools_ms ?? 0;
    return {
      id: r.id,
      label: fmtLabel(r.ts_ms),
      prepMs,
      compactMs,
      backoffMs,
      connectMs,
      waitMs,
      genMs,
      reasoningMs,
      stallMs,
      toolsMs,
      totalMs: prepMs + compactMs + backoffMs + connectMs + waitMs + genMs + toolsMs,
    };
  });
}

/** Per-request token counts (prompt / cached / completion / reasoning). */
export function tokenSeries(rows: LlmRequestSummary[], max = 24): TokenSeriesRow[] {
  return latestRows(rows, max).map((r) => ({
    id: r.id,
    label: fmtLabel(r.ts_ms),
    prompt: r.usage?.prompt ?? 0,
    cached: r.usage?.cached ?? 0,
    completion: r.usage?.completion ?? 0,
    reasoning: r.usage?.reasoning ?? 0,
  }));
}

/** Session-level aggregates over ALL rows (not capped — the ring is bounded). */
export function traceSummary(rows: LlmRequestSummary[]): TraceStatsSummary {
  let totalPrepMs = 0;
  let totalCompactMs = 0;
  let totalBackoffMs = 0;
  let totalConnectMs = 0;
  let totalWaitMs = 0;
  let totalGenMs = 0;
  let totalStallMs = 0;
  let totalToolsMs = 0;
  let totalPrompt = 0;
  let totalCached = 0;
  let totalCompletion = 0;
  const hits: number[] = [];
  for (const r of rows) {
    totalPrepMs += r.prep_ms ?? 0;
    totalCompactMs += r.compact_ms ?? 0;
    totalBackoffMs += r.backoff_ms ?? 0;
    totalConnectMs += r.connect_ms ?? 0;
    totalWaitMs += r.ttft_ms ?? 0;
    totalGenMs += r.generation_ms ?? 0;
    totalStallMs += r.stall_ms ?? 0;
    totalToolsMs += r.tools_ms ?? 0;
    totalPrompt += r.usage?.prompt ?? 0;
    totalCached += r.usage?.cached ?? 0;
    totalCompletion += r.usage?.completion ?? 0;
    const pct = cacheHitPct(r.usage);
    if (pct !== null) hits.push(pct);
  }
  return {
    count: rows.length,
    totalPrepMs,
    totalCompactMs,
    totalBackoffMs,
    totalConnectMs,
    totalWaitMs,
    totalGenMs,
    totalStallMs,
    totalToolsMs,
    totalPrompt,
    totalCached,
    totalCompletion,
    avgTokPerSec:
      totalGenMs > 0
        ? Math.round((totalCompletion / (totalGenMs / 1000)) * 10) / 10
        : null,
    avgCacheHitPct:
      hits.length > 0
        ? Math.round((hits.reduce((a, b) => a + b, 0) / hits.length) * 10) / 10
        : null,
  };
}

/** Comma-grouped integer (e.g. 6000 → "6,000"). */
export function fmtInt(n: number): string {
  return Math.round(n).toLocaleString("en-US");
}

/** A compact per-phase duration: "123ms", "1.2s", "1m 3s". */
export function fmtPhase(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  return fmtDuration(ms);
}

/** The phase-time chart's stacked-segment palette — shared by the chart
 *  columns, the legend, and the tooltip dots so a phase always wears the
 *  same color. `stall` is a SUBSET of `generate` (byte-silence inside the
 *  generation window, rendered as a dithered red hatch inside the generate
 *  segment), so it is not additive — the height math excludes it via
 *  {@link phaseHeightVisibility}. */
export const PHASE_SEGMENTS = [
  { key: "prep", label: "prep", cls: "bg-teal-400/80" },
  { key: "compact", label: "compact", cls: "bg-rose-400/80" },
  { key: "backoff", label: "backoff", cls: "bg-orange-400/80" },
  { key: "connect", label: "connect", cls: "bg-cyan-500/80" },
  { key: "wait", label: "wait", cls: "bg-slate-500/80" },
  { key: "reason", label: "reason", cls: "bg-purple-500/80" },
  { key: "generate", label: "generate", cls: "bg-violet-500/80" },
  { key: "stall", label: "stall", cls: "bg-red-400/80" },
  { key: "tools", label: "tools", cls: "bg-amber-500/80" },
] as const;

/** The token chart's stacked-segment palette — shared by the chart columns,
 *  the legend, and the tooltip dots so a token series always wears the same
 *  color. `cached` is a SUBSET of `prompt` (rendered as a dithered overlay
 *  inside the prompt segment), so it is not additive. */
export const TOKEN_SEGMENTS = [
  { key: "prompt", label: "prompt", cls: "bg-sky-500/70" },
  { key: "cached", label: "cached", cls: "bg-emerald-500/70" },
  { key: "completion", label: "completion", cls: "bg-violet-500/70" },
  { key: "reasoning", label: "reasoning", cls: "bg-purple-500/70" },
] as const;

/** Per-segment show/hide state for one chart, keyed by segment key (the
 *  `key` field of PHASE_SEGMENTS / TOKEN_SEGMENTS). Absent keys are treated
 *  as visible. */
export type SegmentVisibility = Record<string, boolean>;

/** The chart-wide visibility map for one palette given the hidden keys —
 *  every palette key mapped to shown/hidden. Unknown hidden keys (e.g. stale
 *  persisted ones) are ignored: only palette keys ever appear in the map.
 *  The single wiring the chart components use so "hidden" means exactly
 *  "in the set". Pure. */
export function chartVisibility(
  segments: readonly { key: string }[],
  hidden: ReadonlySet<string>,
): SegmentVisibility {
  const v: SegmentVisibility = {};
  for (const s of segments) v[s.key] = !hidden.has(s.key);
  return v;
}

/** The token chart's HEIGHT visibility: `visibility` with `cached` always
 *  EXCLUDED from the sum. `cached` is a non-additive overlay (a subset of
 *  prompt, painted inside the prompt bar), so the height driver
 *  `stackTotal(tokenMsByKey(r), tokenHeightVisibility(v))` must sum
 *  prompt + completion + reasoning — toggling the cached chip stays
 *  height-neutral and hiding prompt leaves completion + reasoning. Pure. */
export function tokenHeightVisibility(visibility: SegmentVisibility): SegmentVisibility {
  return { ...visibility, cached: false };
}

/** The phase chart's HEIGHT visibility: `visibility` with `stall` always
 *  EXCLUDED from the sum. `stall` is a non-additive overlay (byte-silence
 *  inside the generation window, painted as the dithered hatch INSIDE the
 *  generate bar), so the height driver
 *  `stackTotal(phaseMsByKey(r), phaseHeightVisibility(v))` must sum
 *  prep + compact + backoff + connect + wait + reason + generate + tools —
 *  toggling the stall chip stays height-neutral (mirrors
 *  {@link tokenHeightVisibility}). Pure. */
export function phaseHeightVisibility(visibility: SegmentVisibility): SegmentVisibility {
  return { ...visibility, stall: false };
}

/** The phase chart's per-segment values keyed by PHASE_SEGMENTS key — the
 *  single extractor shared by the chart columns and the tooltip rows so a
 *  phase always maps to the same value. `generate` is genMs − reasoningMs:
 *  stall is NOT subtracted — it is byte-silence inside the generation
 *  window, so it counts as generate and renders as the dithered hatch
 *  overlay inside the generate segment (non-additive, like `cached` inside
 *  `prompt`; excluded from height math via {@link phaseHeightVisibility}).
 *  Pure. */
export function phaseMsByKey(r: PhaseSeriesRow): Record<string, number> {
  return {
    prep: r.prepMs,
    compact: r.compactMs,
    backoff: r.backoffMs,
    connect: r.connectMs,
    wait: r.waitMs,
    reason: r.reasoningMs,
    generate: Math.max(0, r.genMs - r.reasoningMs),
    stall: r.stallMs,
    tools: r.toolsMs,
  };
}

/**
 * The stalled share of the generate segment as a percentage (0–100) — the
 * dithered hatch coverage inside the generate bar. Denominator is the
 * generate segment (genMs − reasoningMs) so the hatch reads as "how much of
 * this bar stalled"; clamped to 100 because a stall during the reasoning
 * window is counted in BOTH reasoning_ms and stall_ms, so stall can exceed
 * the generate remainder. 0 when there is no stall or no measurable
 * generate segment. Pure.
 */
export function stallSharePct(r: PhaseSeriesRow): number {
  const generateMs = Math.max(0, r.genMs - r.reasoningMs);
  if (r.stallMs <= 0 || generateMs <= 0) return 0;
  return Math.min(100, Math.round((r.stallMs / generateMs) * 1000) / 10);
}

/** The token chart's per-segment values keyed by TOKEN_SEGMENTS key. Pure. */
export function tokenMsByKey(r: TokenSeriesRow): Record<string, number> {
  return {
    prompt: r.prompt,
    cached: r.cached,
    completion: r.completion,
    reasoning: r.reasoning,
  };
}

/** The segments of one stacked column that actually render: palette order
 *  preserved, only segments that are both visible and non-zero. Pure. */
export function stackSegments(
  segments: readonly { key: string; label: string; cls: string }[],
  msByKey: Record<string, number>,
  visibility: SegmentVisibility,
): { key: string; label: string; cls: string; value: number }[] {
  return segments
    .filter((s) => visibility[s.key] !== false && (msByKey[s.key] ?? 0) > 0)
    .map((s) => ({ key: s.key, label: s.label, cls: s.cls, value: msByKey[s.key] ?? 0 }));
}

/** The visible total of one stacked column — the sum of the visible values
 *  in `msByKey`. Drives the column height and the chart max, so hiding a
 *  tall segment rescales the rest (Relative mode). For maps that contain
 *  non-additive keys (the token chart's `cached`, a subset of `prompt`),
 *  exclude them first via {@link tokenHeightVisibility}. Pure. */
export function stackTotal(msByKey: Record<string, number>, visibility: SegmentVisibility): number {
  let total = 0;
  for (const [key, value] of Object.entries(msByKey)) {
    if (visibility[key] !== false) total += value;
  }
  return total;
}

/** One row of a chart tooltip: a label, its formatted value, and an optional
 *  color-dot class matching the chart segment. */
export interface TipRow {
  label: string;
  value: string;
  /** Tailwind bg-* class for the color dot (matches the chart segment). */
  cls?: string;
}

/** Tooltip rows for the phase-time chart: total first, then one row per
 *  phase in stack order (zero phases render "—" so the layout stays stable
 *  across columns). Pure. */
export function phaseTipRows(r: PhaseSeriesRow): TipRow[] {
  const ms = phaseMsByKey(r);
  return [
    { label: "total", value: fmtPhase(r.totalMs) },
    ...PHASE_SEGMENTS.map((s) => ({
      label: s.label,
      value: ms[s.key] > 0 ? fmtPhase(ms[s.key]) : "—",
      cls: s.cls,
    })),
  ];
}

/** Tooltip rows for the token chart — colors derive from TOKEN_SEGMENTS so
 *  a palette edit can never desync the tooltip dots from the chart/legend.
 *  Pure. */
export function tokenTipRows(r: TokenSeriesRow): TipRow[] {
  const cachedPct =
    r.prompt > 0 ? Math.min(100, Math.round((r.cached / r.prompt) * 1000) / 10) : 0;
  const value: Record<string, string> = {
    prompt: fmtInt(r.prompt),
    cached: r.cached > 0 ? `${fmtInt(r.cached)} (${fmtPct(cachedPct)}%)` : "—",
    completion: r.completion > 0 ? fmtInt(r.completion) : "—",
    reasoning: r.reasoning > 0 ? fmtInt(r.reasoning) : "—",
  };
  return TOKEN_SEGMENTS.map((s) => ({ label: s.label, value: value[s.key], cls: s.cls }));
}
