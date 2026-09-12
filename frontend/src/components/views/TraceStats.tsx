// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { BarChart3 } from "lucide-react";
import { useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import type { LlmRequestSummary } from "../../lib/types";
import { fmtPct } from "../../lib/format";
import {
  PHASE_SEGMENTS,
  TOKEN_SEGMENTS,
  chartVisibility,
  columnHeightPct,
  fmtPhase,
  phaseHeightVisibility,
  phaseMsByKey,
  phaseSeries,
  phaseTipRows,
  stallSharePct,
  stackSegments,
  stackTotal,
  tokenHeightVisibility,
  tokenMsByKey,
  tokenSeries,
  tokenTipRows,
  traceSummary,
  type PhaseSeriesRow,
  type ScaleMode,
  type TipRow,
  type TokenSeriesRow,
} from "../../lib/traceStats";

/**
 * The stats section at the TOP of the Trace tab (resizable via the drag
 * splitter below it): a summary strip plus two stacked vertical column charts
 * derived from the same polled request list the rows use — per-request phase
 * time (prep/compact/backoff/connect/wait/reason/generate/tools stacked
 * columns, with the stalled share as a dithered red overlay inside the
 * generate column) and
 * per-request tokens (prompt with a dithered cached overlay showing the
 * cache-hit rate / completion / reasoning). Columns run oldest → newest
 * left-to-right, so the newest request is always the rightmost column. A
 * Fill/Relative toggle scales bar heights: Fill normalizes every column to
 * the full chart height (proportions within one request); Relative makes
 * heights comparable across requests (a 2-minute request's column is 4×
 * taller than a 30-second one). The legend chips under each chart title are
 * toggles: clicking one hides/shows that series in that chart (persisted
 * across sessions via localStorage `tracestats.hiddenSegments`), and column
 * maxima recompute over the visible series only — so hiding a dominant
 * segment zooms the rest (most useful in Relative mode). The `cached` chip
 * toggles only the dithered overlay inside the prompt segment, and the
 * `stall` chip only the overlay inside the generate segment. Pure
 * HTML/flex columns — no chart dependency, same palette as the rest of the
 * app, live-updating with the 1.5s trace poll.
 */

/** One entry of a chart legend: segment key, label, and swatch color class. */
interface LegendItem {
  key: string;
  label: string;
  cls: string;
}

/**
 * A small legend row for one chart card. When `hidden`/`onToggle` are
 * provided, every chip is a toggle button: clicking shows/hides that series
 * in the chart (off = faded swatch + struck-through label). A hidden series
 * keeps its legend slot so the layout never jumps.
 */
function Legend({
  items,
  hidden,
  onToggle,
}: {
  items: LegendItem[];
  /** Off (hidden) segment keys — omit to render plain, non-clickable chips. */
  hidden?: ReadonlySet<string>;
  /** Called with the segment key on click. */
  onToggle?: (key: string) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      {items.map((it) => {
        const off = hidden?.has(it.key) ?? false;
        const chip = (
          <>
            <span className={`h-1.5 w-1.5 rounded-sm ${it.cls} ${off ? "opacity-30" : ""}`} />
            <span className={off ? "line-through" : undefined}>{it.label}</span>
          </>
        );
        if (!onToggle) {
          return (
            <span key={it.key} className="flex items-center gap-1 text-[0.62em] text-slate-500">
              {chip}
            </span>
          );
        }
        return (
          <button
            key={it.key}
            type="button"
            aria-pressed={!off}
            aria-label={`${off ? "Show" : "Hide"} ${it.label}`}
            title={off ? `Show ${it.label}` : `Hide ${it.label} (click to show/hide)`}
            onClick={() => onToggle(it.key)}
            className={`flex cursor-pointer items-center gap-1 rounded px-0.5 text-[0.62em] transition-opacity hover:opacity-80 ${
              off ? "text-slate-600" : "text-slate-500"
            }`}
          >
            {chip}
          </button>
        );
      })}
    </div>
  );
}

/** localStorage key for the Fill/Relative column-scale toggle. */
const SCALE_MODE_KEY = "tracestats.scaleMode";

/** Read the persisted scale mode (default "relative" when absent/corrupt). */
function storedScaleMode(): ScaleMode {
  return localStorage.getItem(SCALE_MODE_KEY) === "fill" ? "fill" : "relative";
}

/** localStorage key for the per-chart hidden legend segments (JSON object:
 *  {"phase": ["wait", ...], "token": ["reasoning", ...]} — sparse lists of
 *  HIDDEN keys, so a segment added later defaults to visible). */
const HIDDEN_SEGMENTS_KEY = "tracestats.hiddenSegments";

/** Read the persisted hidden-segment lists (tolerates corrupt/missing JSON). */
function storedHiddenSegments(): { phase: string[]; token: string[] } {
  try {
    const raw = localStorage.getItem(HIDDEN_SEGMENTS_KEY);
    if (!raw) return { phase: [], token: [] };
    const obj = (JSON.parse(raw) ?? {}) as Record<string, unknown>;
    const pick = (v: unknown): string[] =>
      Array.isArray(v) ? v.filter((k): k is string => typeof k === "string") : [];
    return { phase: pick(obj.phase), token: pick(obj.token) };
  } catch {
    return { phase: [], token: [] };
  }
}

/** Persist the hidden-segment lists (best-effort — private-mode quota). */
function storeHiddenSegments(hidden: { phase: string[]; token: string[] }): void {
  try {
    localStorage.setItem(HIDDEN_SEGMENTS_KEY, JSON.stringify(hidden));
  } catch {
    // Storage unavailable — the toggle still works for this session.
  }
}

/**
 * One chart column: a faint full-height track with the bar anchored to the
 * bottom, plus the request's time label in tiny vertical text underneath so
 * up to 24 columns stay legible. The full values live in a CUSTOM tooltip —
 * multiline, tab-aligned, color-dotted to match the segments — rendered via a
 * portal to <body> on hover: a portal because the chart containers clip
 * overflow, and a custom tooltip because a native `title` can't do
 * multiline/aligned layout (user request 2026-08-22).
 */
function Column({
  label,
  tipHeader,
  tipRows,
  heightPct,
  children,
}: {
  /** Time label under the column (HH:MM:SS). */
  label: string;
  /** Tooltip header line: request id + time label. */
  tipHeader: string;
  /** Tooltip rows (label/value/color) from the traceStats builders. */
  tipRows: TipRow[];
  /** Bar height as a percentage of the chart body (0 = track only). */
  heightPct: number;
  /** The stacked segments — the first child renders at the bottom. */
  children: ReactNode;
}) {
  // Anchor rect captured once on hover entry; the tooltip renders fixed above
  // the column (flips below near the viewport top).
  const [anchor, setAnchor] = useState<{ cx: number; top: number; bottom: number } | null>(null);
  // Screen-reader access for the removed native title: role="img" makes the
  // aria-label announce the column's flat one-sentence summary.
  const flat = `${tipHeader} — ${tipRows.map((r) => `${r.label} ${r.value}`).join(", ")}`;
  return (
    <div
      className="flex h-full min-w-[14px] flex-1 flex-col items-center"
      role="img"
      aria-label={flat}
      onMouseEnter={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        setAnchor({ cx: rect.left + rect.width / 2, top: rect.top, bottom: rect.bottom });
      }}
      onMouseLeave={() => setAnchor(null)}
    >
      {/* The faint full-height track keeps empty/zero columns visible; the
          bar itself is anchored to the bottom. */}
      <div className="flex min-h-0 w-full flex-1 items-end overflow-hidden rounded-sm bg-bg-tertiary/40">
        <div
          className="flex w-full flex-col-reverse overflow-hidden rounded-sm"
          style={{ height: `${heightPct}%` }}
        >
          {children}
        </div>
      </div>
      <span
        className="mt-0.5 shrink-0 text-[0.55em] text-slate-500"
        style={{ writingMode: "vertical-rl" }}
      >
        {label}
      </span>
      {anchor &&
        (() => {
          // Clamp the center so the tooltip (≈200px wide: min-w 170 +
          // padding) can't clip the viewport edges — the newest column is
          // always rightmost, so its center sits near the right edge.
          const cx = Math.min(Math.max(anchor.cx, 108), window.innerWidth - 108);
          // 170 ≈ the tallest tooltip's height (8 phase rows + header);
          // closer to the viewport top than that, flip the card below.
          const above = anchor.top > 170;
          return createPortal(
            <div
              className="pointer-events-none fixed z-50 -translate-x-1/2"
              style={
                above
                  ? { left: cx, bottom: window.innerHeight - anchor.top + 6 }
                  : { left: cx, top: anchor.bottom + 6 }
              }
            >
            <div className="min-w-[170px] rounded-md border border-border bg-bg-secondary px-2.5 py-1.5 text-[0.65rem] shadow-xl">
              <div className="mb-1 border-b border-border pb-1 font-semibold text-slate-300">
                {tipHeader}
              </div>
              <div className="flex flex-col gap-0.5">
                {tipRows.map((r) => (
                  <div key={r.label} className="flex items-center justify-between gap-4">
                    <span className="flex items-center gap-1.5 text-slate-400">
                      {r.cls && <span className={`h-1.5 w-1.5 rounded-sm ${r.cls}`} />}
                      {r.label}
                    </span>
                    <span className="font-mono text-slate-200">{r.value}</span>
                  </div>
                ))}
              </div>
            </div>
          </div>,
            document.body,
          );
        })()}
    </div>
  );
}

/**
 * Stacked per-request phase-time columns (prep / compact / backoff / connect /
 * wait / reason / generate / tools). Legend chips toggle phases: a hidden phase
 * drops out of the columns AND the max, so the rest rescales (Relative mode
 * gains resolution) — except `stall`, which is a subset of `generate`
 * (byte-silence inside the generation window): it renders as a dithered red
 * hatch INSIDE the generate segment and its chip toggles only the overlay,
 * height-neutral (like `cached` in the token chart). Hiding every phase
 * swaps the body for a hint line.
 */
function PhaseChart({
  rows,
  mode,
  hidden,
  onToggle,
}: {
  rows: PhaseSeriesRow[];
  mode: ScaleMode;
  /** Hidden segment keys (the persisted phase list). */
  hidden: ReadonlySet<string>;
  /** Show/hide one phase segment (persisted via TraceStats). */
  onToggle: (key: string) => void;
}) {
  const visibility = chartVisibility(PHASE_SEGMENTS, hidden);
  // `stall` is a subset of `generate` (byte-silence inside the generation
  // window) — it is not a stacked segment; it renders as the dithered red
  // hatch INSIDE the generate bar, and its chip is height-neutral.
  const stacked = PHASE_SEGMENTS.filter((s) => s.key !== "stall");
  // The empty-state hint keys off the STACKED segments (mirrors the token
  // chart): hiding every stacked phase shows the hint even if the stall
  // overlay chip is still on — the overlay alone renders no column.
  const allHidden = stacked.every((s) => hidden.has(s.key));
  const heightVisibility = phaseHeightVisibility(visibility);
  const hasData = rows.some((r) => stackTotal(phaseMsByKey(r), heightVisibility) > 0);
  const maxTotal = Math.max(0, ...rows.map((r) => stackTotal(phaseMsByKey(r), heightVisibility)));
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col rounded border border-border p-2">
      <div className="flex items-center justify-between gap-2">
        <span className="text-[0.68em] font-semibold uppercase tracking-wide text-slate-400">
          Phase time per request
        </span>
        <Legend
          items={PHASE_SEGMENTS.map(({ key, label, cls }) => ({ key, label, cls }))}
          hidden={hidden}
          onToggle={onToggle}
        />
      </div>
      {!hasData ? (
        <p className="mt-2 text-[0.68em] text-slate-500">
          {allHidden
            ? "All series hidden — click a legend chip to show one."
            : "No phase timings recorded yet."}
        </p>
      ) : (
        <div className="mt-2 flex min-h-0 flex-1 items-end gap-0.5 overflow-x-auto">
          {rows.map((r) => {
            // One shared extractor feeds the columns AND the tooltip dots.
            const msByKey = phaseMsByKey(r);
            const segments = stackSegments(stacked, msByKey, visibility);
            // Sliver guard: a non-zero request never drops below 2% height.
            // The height driver EXCLUDES stall (non-additive overlay).
            const total = stackTotal(msByKey, heightVisibility);
            const heightPct = total > 0 ? Math.max(2, columnHeightPct(total, maxTotal, mode)) : 0;
            return (
              <Column
                key={r.id}
                label={r.label}
                heightPct={heightPct}
                tipHeader={`#${r.id} · ${r.label}`}
                tipRows={phaseTipRows(r)}
              >
                {segments.map((s) =>
                  s.key === "generate" ? (
                    /* Generate at the top of the stack; the stalled share
                       overlays it as a dithered red hatch — coverage =
                       stall/generate, so the stall rate reads as how much of
                       the generate bar is hatched (violet shows through the
                       stripe gaps). The `stall` legend chip toggles ONLY this
                       overlay (height-neutral, like `cached`). */
                    <div
                      key={s.key}
                      className="relative bg-violet-500/80"
                      style={{ flexGrow: s.value, flexBasis: 0 }}
                    >
                      {!hidden.has("stall") && r.stallMs > 0 && (
                        <div
                          className="absolute inset-x-0 bottom-0"
                          style={{
                            height: `${stallSharePct(r)}%`,
                            backgroundColor: "rgba(248,113,113,0.22)",
                            backgroundImage:
                              "repeating-linear-gradient(45deg, rgba(248,113,113,0.85) 0 2px, transparent 2px 5px)",
                          }}
                        />
                      )}
                    </div>
                  ) : (
                    <div
                      key={s.key}
                      className={s.cls}
                      style={{ flexGrow: s.value, flexBasis: 0 }}
                    />
                  ),
                )}
              </Column>
            );
          })}
        </div>
      )}
    </div>
  );
}

/**
 * Per-request token columns (prompt with cached overlay / completion /
 * reasoning). `cached` is an overlay INSIDE the prompt segment, so its chip
 * toggles only the dithered hatch — the prompt bar always counts toward the
 * height. Other hidden series drop from the columns AND the max so the rest
 * rescales (Relative mode gains resolution).
 */
function TokenChart({
  rows,
  mode,
  hidden,
  onToggle,
}: {
  rows: TokenSeriesRow[];
  mode: ScaleMode;
  /** Hidden segment keys (the persisted token list). */
  hidden: ReadonlySet<string>;
  /** Show/hide one token series (persisted via TraceStats). */
  onToggle: (key: string) => void;
}) {
  const visibility = chartVisibility(TOKEN_SEGMENTS, hidden);
  // The columns stack prompt → completion → reasoning; `cached` renders as
  // the dithered overlay inside the prompt segment, never as its own slice.
  const stacked = TOKEN_SEGMENTS.filter((s) => s.key !== "cached");
  // `cached` is a non-additive overlay (a subset of prompt painted INSIDE
  // the prompt bar) — the height math always EXCLUDES it, so totals and
  // maxima sum prompt + completion + reasoning (the pre-toggle
  // tokenStackTotal semantics) and toggling the cached chip is
  // height-neutral (review High 1).
  const heightVisibility = tokenHeightVisibility(visibility);
  const allHidden = stacked.every((s) => hidden.has(s.key));
  const hasData = rows.some((r) => stackTotal(tokenMsByKey(r), heightVisibility) > 0);
  const maxStack = Math.max(0, ...rows.map((r) => stackTotal(tokenMsByKey(r), heightVisibility)));
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col rounded border border-border p-2">
      <div className="flex items-center justify-between gap-2">
        <span className="text-[0.68em] font-semibold uppercase tracking-wide text-slate-400">
          Tokens per request
        </span>
        <Legend
          items={TOKEN_SEGMENTS.map(({ key, label, cls }) => ({ key, label, cls }))}
          hidden={hidden}
          onToggle={onToggle}
        />
      </div>
      {!hasData ? (
        <p className="mt-2 text-[0.68em] text-slate-500">
          {allHidden
            ? "All series hidden — click a legend chip to show one."
            : "No usage reported by the provider yet."}
        </p>
      ) : (
        <div className="mt-2 flex min-h-0 flex-1 items-end gap-0.5 overflow-x-auto">
          {rows.map((r) => {
            const total = stackTotal(tokenMsByKey(r), heightVisibility);
            const heightPct = total > 0 ? Math.max(2, columnHeightPct(total, maxStack, mode)) : 0;
            const cachedPct = r.prompt > 0 ? Math.min(100, (r.cached / r.prompt) * 100) : 0;
            return (
              <Column
                key={r.id}
                label={r.label}
                heightPct={heightPct}
                tipHeader={`#${r.id} · ${r.label}`}
                tipRows={tokenTipRows(r)}
              >
                {stackSegments(stacked, tokenMsByKey(r), visibility).map((s) =>
                  s.key === "prompt" ? (
                    /* Prompt at the bottom; the cached share overlays it as a
                       dithered emerald hatch — coverage = cached/prompt, so
                       the cache-hit rate reads as how much of the prompt bar
                       is hatched (sky-blue shows through the stripe gaps).
                       The `cached` legend chip toggles ONLY this overlay. */
                    <div
                      key={s.key}
                      className="relative bg-sky-500/70"
                      style={{ flexGrow: s.value, flexBasis: 0 }}
                    >
                      {!hidden.has("cached") && r.cached > 0 && (
                        <div
                          className="absolute inset-x-0 bottom-0"
                          style={{
                            height: `${cachedPct}%`,
                            backgroundColor: "rgba(16,185,129,0.22)",
                            backgroundImage:
                              "repeating-linear-gradient(45deg, rgba(16,185,129,0.85) 0 2px, transparent 2px 5px)",
                          }}
                        />
                      )}
                    </div>
                  ) : (
                    <div
                      key={s.key}
                      className={s.cls}
                      style={{ flexGrow: s.value, flexBasis: 0 }}
                    />
                  ),
                )}
              </Column>
            );
          })}
        </div>
      )}
    </div>
  );
}

/**
 * The Trace tab's top stats section (resizable via the drag splitter below
 * it). `rows` is the oldest-first list straight from the backend poll (the
 * same `requests` state the rows render from); the charts derive their
 * series from it. `height` is the section's pixel height, owned + persisted
 * by LlmTraceView.
 */
export function TraceStats({ rows, height }: { rows: LlmRequestSummary[]; height: number }) {
  // Fill/Relative column-scale toggle — persisted across sessions.
  const [mode, setMode] = useState<ScaleMode>(storedScaleMode);
  // Per-chart hidden legend segments ({"phase": [...], "token": [...]}) —
  // persisted so a filtered view survives restarts.
  const [hiddenLists, setHiddenLists] = useState(storedHiddenSegments);
  if (rows.length === 0) return null;
  const phases = phaseSeries(rows);
  const tokens = tokenSeries(rows);
  const summary = traceSummary(rows);
  const selectMode = (m: ScaleMode) => {
    setMode(m);
    localStorage.setItem(SCALE_MODE_KEY, m);
  };
  /** Show/hide one legend series in one chart (persisted immediately). */
  const toggleSegment = (chart: "phase" | "token", key: string) => {
    // Compute from the render-fresh state — NOT inside the setState updater
    // (updaters must stay pure; the component re-renders every poll anyway,
    // so the closure is never stale for a click handler).
    const list = hiddenLists[chart].includes(key)
      ? hiddenLists[chart].filter((k) => k !== key)
      : [...hiddenLists[chart], key];
    const next = { ...hiddenLists, [chart]: list };
    setHiddenLists(next);
    storeHiddenSegments(next);
  };
  const modeBtn = (m: ScaleMode, label: string) => (
    <button
      onClick={() => selectMode(m)}
      className={`rounded px-1.5 py-0.5 text-[0.62em] font-medium normal-case tracking-normal transition-colors ${
        mode === m
          ? "bg-cyan-500/20 text-cyan-300"
          : "bg-bg-tertiary text-slate-400 hover:text-slate-200"
      }`}
    >
      {label}
    </button>
  );
  return (
    <div className="flex shrink-0 flex-col" style={{ height: `${height}px` }}>
      <div className="flex items-center justify-between gap-2 border-b border-border px-2 py-1">
        <div className="flex items-center gap-2">
          <span className="flex items-center gap-1.5 text-[0.68em] font-semibold uppercase tracking-wide text-slate-400">
            <BarChart3 className="h-3 w-3 text-cyan-400" /> Stats
          </span>
          <span
            className="flex items-center gap-0.5"
            title="Column scale — Fill: every column fills the chart height (proportions within one request). Relative: heights are proportional across requests (a 2-minute request's column is 4× taller than a 30-second one)."
          >
            {modeBtn("fill", "Fill")}
            {modeBtn("relative", "Relative")}
          </span>
        </div>
        <div className="flex items-center gap-3 overflow-x-auto font-mono text-[0.65em] text-slate-500">
          <span title="requests in the ring buffer">{summary.count} reqs</span>
          <span title="total local prep time (prompt build, token counting, recall)">
            Σ prep {fmtPhase(summary.totalPrepMs)}
          </span>
          <span title="total auto-compaction time (summarization calls)">
            Σ compact {fmtPhase(summary.totalCompactMs)}
          </span>
          <span title="total retry-backoff sleep (the 1s/2s waits between failed attempts)">
            Σ backoff {fmtPhase(summary.totalBackoffMs)}
          </span>
          <span title="total connect time (record created → POST response headers)">
            Σ connect {fmtPhase(summary.totalConnectMs)}
          </span>
          <span title="total time-to-first-token">Σ wait {fmtPhase(summary.totalWaitMs)}</span>
          <span title="total generation time (first → last chunk)">
            Σ gen {fmtPhase(summary.totalGenMs)}
          </span>
          <span title="total mid-stream stall time (byte-silence gaps inside the generation window)">
            Σ stall {fmtPhase(summary.totalStallMs)}
          </span>
          <span title="total tools phase (stream end → batch done)">
            Σ tools {fmtPhase(summary.totalToolsMs)}
          </span>
          {summary.avgTokPerSec !== null && (
            <span title="completion tokens / total generation time">
              ↓ {summary.avgTokPerSec.toFixed(1)}/s
            </span>
          )}
          {summary.avgCacheHitPct !== null && (
            <span className="text-emerald-400" title="mean cache-hit rate across requests">
              {fmtPct(summary.avgCacheHitPct)}% cache
            </span>
          )}
        </div>
      </div>
      <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-hidden p-2">
        <PhaseChart
          rows={phases}
          mode={mode}
          hidden={new Set(hiddenLists.phase)}
          onToggle={(k) => toggleSegment("phase", k)}
        />
        <TokenChart
          rows={tokens}
          mode={mode}
          hidden={new Set(hiddenLists.token)}
          onToggle={(k) => toggleSegment("token", k)}
        />
      </div>
    </div>
  );
}
