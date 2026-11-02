/**
 * The Trace page's chart data.
 *
 * `TraceCharts.svelte` draws three charts from the one filtered row set the
 * list shows: where a call's time went (a stacked phase bar per call), what
 * its tokens went to (four non-overlapping segments per call), and what calls
 * cost over time (auto minute/hour/day buckets stacked by agent or model).
 *
 * Everything here is pure so the page and the compact side tab cannot drift,
 * and so the arithmetic is testable without a browser. All money is integer
 * micro-USD from the shared pricing helper; a call with no price is never
 * rendered as zero dollars.
 */

import { dayLabel, formatEstimate, shortModelLabel } from "./cost";
import { traceClock, traceStatus } from "./trace";
import type { TraceGuardCut, TraceStatus, TraceUsage } from "./trace";

// ---------------------------------------------------------------------------
// Row contract — mirrors the frozen `TraceRow` wire type (step B/C owner)
// ---------------------------------------------------------------------------

/**
 * Who made a call. Mirrors the kernel's `TraceAttribution`.
 *
 * `actor_kind` and `purpose` are widened to `string` on purpose: the contract's
 * literals are `lead | worker | subagent | system` and `turn | compaction |
 * memory_consolidation | routing | other`, but a new kernel value must render
 * as an unknown actor rather than break the charts.
 */
export interface TraceAttribution {
  actor_kind: string;
  agent_id: string;
  session_id: string;
  agent_name: string;
  worker_id?: string | null;
  plan_id?: string | null;
  plan_step?: number | null;
  turn_id?: string | null;
  purpose: string;
}

/**
 * One trace row as the charts consume it: `LlmRequestSummary` plus the
 * Mnemo2-only pricing/attribution fields.
 *
 * The attribution and pricing fields are optional so this module also compiles
 * against the pre-step-B row shape (and preview fixtures); a row without them
 * reads as Unattributed/Unpriced, never as the lead or as free.
 */
export interface TraceRow {
  id: number;
  /** The capture run this row belongs to (part of the stable row key). */
  run_id?: string;
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
  attribution?: TraceAttribution | null;
  cost_micros?: number | null;
  priced?: boolean;
  retained?: boolean;
}

/** The stable key of one row: the capture run plus the monotonic id. */
function rowKey(row: TraceRow): string {
  return `${row.run_id ?? "run"}:${row.id}`;
}

/** Rows oldest-first, so every chart reads left-to-right as a timeline. */
function ordered(rows: readonly TraceRow[]): TraceRow[] {
  return [...rows].sort((a, b) => a.ts_ms - b.ts_ms || a.id - b.id);
}

/** A non-negative milliseconds reading (`null`/negative reports read as 0). */
function ms(value: number | null | undefined): number {
  return typeof value === "number" && Number.isFinite(value) ? Math.max(0, value) : 0;
}

/** Prompt + completion: cached input stays inside prompt, reasoning in completion. */
function rowTokens(row: TraceRow): number {
  const prompt = ms(row.usage?.prompt);
  const completion = ms(row.usage?.completion);
  return prompt + completion;
}

// ---------------------------------------------------------------------------
// Timing chart — one stacked phase bar per call
// ---------------------------------------------------------------------------

/**
 * The six stacked timing phases, in chronological order from the bottom of a
 * column: retry wait, setup, connect, first token, generation, tools.
 *
 * Two further windows are subsets of a stacked phase and are painted as a
 * hatch inside it, never as extra height: compaction inside setup and stall
 * inside generation. That containment is why the columns are a phase breakdown
 * and never one additive "total time".
 */
export type TimingSegmentKey =
  | "retry"
  | "setup"
  | "connect"
  | "firstToken"
  | "generation"
  | "tools";

/** One stacked timing phase: the friendly label and its hover explanation. */
export interface TimingSegment {
  key: TimingSegmentKey;
  label: string;
  hint: string;
}

/** The stacked timing phases in stack order, shared by the chart and legend. */
export const TIMING_SEGMENTS: readonly TimingSegment[] = [
  {
    key: "retry",
    label: "Retry wait",
    hint: "The pause before this retry attempt — waiting, not part of Connect.",
  },
  {
    key: "setup",
    label: "Setup",
    hint: "Local work before the request: routing, token accounting, recall and prompt building. Includes compaction.",
  },
  {
    key: "connect",
    label: "Connect",
    hint: "From creating the request to the response headers: upload, network and server acknowledgement.",
  },
  {
    key: "firstToken",
    label: "First token",
    hint: "From the response headers to the first streamed chunk.",
  },
  {
    key: "generation",
    label: "Generation",
    hint: "From the first chunk to the final response. Includes model reasoning and stalls.",
  },
  {
    key: "tools",
    label: "Tools",
    hint: "Time running the tools this response asked for, after the response finished.",
  },
];

/** One call's timing column. */
export interface TimingColumn {
  /** Stable chart key: run id plus the monotonic request id. */
  key: string;
  id: number;
  tsMs: number;
  /** Local start time (`14:32:08`) under the column. */
  clock: string;
  /** Setup total (`prep_ms`), compaction included. */
  setupMs: number;
  /** Compaction inside setup; a subset, shown as a hatch. */
  compactionMs: number;
  retryMs: number;
  connectMs: number;
  /** `ttft_ms` after the headers part was removed; never negative. */
  firstTokenMs: number;
  /** Generation total (`generation_ms`), stalls included. */
  generationMs: number;
  /** Stalls inside generation; a subset, shown as a hatch. */
  stallMs: number;
  toolsMs: number;
  status: TraceStatus;
  /** The sum of all six stacked phases (overlays excluded). */
  stackMs: number;
  /** The sum of the visible stacked phases after legend hides. */
  visibleMs: number;
  /** Bar height against the tallest visible column (0..1). */
  heightFraction: number;
}

/** Everything the Timing tab renders for one row set. */
export interface TimingChart {
  columns: TimingColumn[];
  /** The tallest visible stack (ms); 0 when nothing has a timing yet. */
  maxVisibleMs: number;
  /** Whether any column carries a measurable phase. */
  hasData: boolean;
}

/** The six stacked phase values of one column, keyed by segment key. */
export function timingValues(column: TimingColumn): Record<TimingSegmentKey, number> {
  return {
    retry: column.retryMs,
    setup: column.setupMs,
    connect: column.connectMs,
    firstToken: column.firstTokenMs,
    generation: column.generationMs,
    tools: column.toolsMs,
  };
}

/**
 * Build the timing columns from the filtered rows.
 *
 * The phase-attribution spec is applied exactly and nothing is double-counted:
 * - Setup is `prep_ms`, which CONTAINS compaction; compaction is a hatch inside
 *   it and is never added to the stack.
 * - Connect is the headers wait; First token is `ttft_ms − connect_ms`, so the
 *   first-token wait follows the headers instead of repeating the connect.
 * - Generation is `generation_ms`, which CONTAINS stalls; stalls are a hatch
 *   inside it and never extra width.
 * - Retry wait precedes the attempt; Tools run after the response. The column
 *   is a phase breakdown, not one wall-clock total.
 *
 * `hidden` holds legend keys the user turned off; hidden phases drop out of the
 * height driver as well as the stack.
 */
export function timingChart(
  rows: readonly TraceRow[],
  hidden: ReadonlySet<string> = new Set(),
): TimingChart {
  const columns = ordered(rows).map((row): TimingColumn => {
    const setupMs = ms(row.prep_ms);
    const compactionMs = Math.min(setupMs, ms(row.compact_ms));
    const retryMs = ms(row.backoff_ms);
    const connectMs = ms(row.connect_ms);
    const firstTokenMs = Math.max(0, ms(row.ttft_ms) - connectMs);
    const generationMs = ms(row.generation_ms);
    const stallMs = Math.min(generationMs, ms(row.stall_ms));
    const toolsMs = ms(row.tools_ms);
    const values = {
      retry: retryMs,
      setup: setupMs,
      connect: connectMs,
      firstToken: firstTokenMs,
      generation: generationMs,
      tools: toolsMs,
    };
    const stackMs = TIMING_SEGMENTS.reduce((sum, segment) => sum + values[segment.key], 0);
    const visibleMs = TIMING_SEGMENTS.reduce(
      (sum, segment) => (hidden.has(segment.key) ? sum : sum + values[segment.key]),
      0,
    );
    return {
      key: rowKey(row),
      id: row.id,
      tsMs: row.ts_ms,
      clock: traceClock(row.ts_ms),
      setupMs,
      compactionMs,
      retryMs,
      connectMs,
      firstTokenMs,
      generationMs,
      stallMs,
      toolsMs,
      status: traceStatus(row),
      stackMs,
      visibleMs,
      heightFraction: 0,
    };
  });
  const maxVisibleMs = columns.reduce((max, column) => Math.max(max, column.visibleMs), 0);
  for (const column of columns) {
    column.heightFraction = maxVisibleMs > 0 ? column.visibleMs / maxVisibleMs : 0;
  }
  return {
    columns,
    maxVisibleMs,
    hasData: columns.some((column) => column.stackMs > 0),
  };
}

// ---------------------------------------------------------------------------
// Token chart — four non-overlapping segments per call
// ---------------------------------------------------------------------------

/** The four token segments; every token is counted exactly once. */
export type TokenSegmentKey =
  | "uncachedInput"
  | "cachedInput"
  | "visibleOutput"
  | "reasoningOutput";

/** One token segment: the friendly label and its hover explanation. */
export interface TokenSegment {
  key: TokenSegmentKey;
  label: string;
  hint: string;
}

/** The token segments in stack order, shared by the chart and legend. */
export const TOKEN_SEGMENTS: readonly TokenSegment[] = [
  {
    key: "uncachedInput",
    label: "Input",
    hint: "Input tokens the provider did not serve from cache.",
  },
  {
    key: "cachedInput",
    label: "Cached input",
    hint: "Input tokens the provider says it reused from its prompt cache.",
  },
  {
    key: "visibleOutput",
    label: "Output",
    hint: "Output tokens you can read in the response.",
  },
  {
    key: "reasoningOutput",
    label: "Reasoning",
    hint: "Output tokens the model spent thinking; included in output, not added on top.",
  },
];

/** One call's token column. */
export interface TokenColumn {
  key: string;
  id: number;
  tsMs: number;
  clock: string;
  /** Reported prompt tokens. */
  prompt: number;
  /** Reported cached prompt tokens. */
  cached: number;
  /** `prompt − cached`, saturated at zero. */
  uncachedInput: number;
  /** Reported completion tokens. */
  completion: number;
  /** Reported reasoning tokens. */
  reasoning: number;
  /** `completion − reasoning`, saturated at zero. */
  visibleOutput: number;
  /** `prompt + completion`; reasoning is never added twice. */
  totalTokens: number;
  /** This call's cache reuse (0..100), or null when it reported no prompt. */
  cachePct: number | null;
  /** Whether the provider reported usage for this call at all. */
  hasUsage: boolean;
  /** The sum of the visible segments after legend hides. */
  visibleTokens: number;
  /** Bar height against the tallest visible column (0..1). */
  heightFraction: number;
}

/** The cache-reuse summary over a filtered set. */
export interface TokenSummary {
  /** Calls that reported usage. */
  calls: number;
  /** Calls with no reported usage (rendered as empty columns). */
  missingUsage: number;
  promptTotal: number;
  cachedTotal: number;
  completionTotal: number;
  reasoningTotal: number;
  /** `sum(cached) / sum(prompt)` as a percentage; null when no prompt tokens. */
  cacheReusePct: number | null;
}

/** Everything the Tokens tab renders for one row set. */
export interface TokenChart {
  columns: TokenColumn[];
  summary: TokenSummary;
  /** The tallest visible column (tokens). */
  maxVisibleTokens: number;
  /** Whether any visible segment carries tokens. */
  hasData: boolean;
}

/**
 * A cache-reuse percentage from token totals (0..100, one decimal), or null
 * when the prompt total is zero. Clamped at 100 so a provider that reports
 * more cached tokens than prompt tokens never renders an over-100% bar.
 */
export function cacheReusePct(cached: number, prompt: number): number | null {
  if (!Number.isFinite(cached) || !Number.isFinite(prompt) || prompt <= 0) return null;
  return Math.round(Math.min(1, Math.max(0, cached) / prompt) * 1000) / 10;
}

/** The four token segment values of one column, keyed by segment key. */
export function tokenValues(column: TokenColumn): Record<TokenSegmentKey, number> {
  return {
    uncachedInput: column.uncachedInput,
    cachedInput: column.cached,
    visibleOutput: column.visibleOutput,
    reasoningOutput: column.reasoning,
  };
}

/**
 * Build the token columns from the filtered rows.
 *
 * The four segments are disjoint: `prompt − cached`, `cached`, `completion −
 * reasoning`, and `reasoning`, each saturated at zero for inconsistent provider
 * reports. `cached` and `reasoning` are clamped to their parents so the total
 * is always exactly `prompt + completion` and reasoning is never added twice.
 */
export function tokenChart(
  rows: readonly TraceRow[],
  hidden: ReadonlySet<string> = new Set(),
): TokenChart {
  let calls = 0;
  let missingUsage = 0;
  let promptTotal = 0;
  let cachedTotal = 0;
  let completionTotal = 0;
  let reasoningTotal = 0;
  const columns = ordered(rows).map((row): TokenColumn => {
    const usage = row.usage;
    const prompt = ms(usage?.prompt);
    const cached = Math.min(prompt, ms(usage?.cached));
    const completion = ms(usage?.completion);
    const reasoning = Math.min(completion, ms(usage?.reasoning));
    const uncachedInput = Math.max(0, prompt - cached);
    const visibleOutput = Math.max(0, completion - reasoning);
    if (usage === null) missingUsage += 1;
    else {
      calls += 1;
      promptTotal += prompt;
      cachedTotal += cached;
      completionTotal += completion;
      reasoningTotal += reasoning;
    }
    const values = {
      uncachedInput,
      cachedInput: cached,
      visibleOutput,
      reasoningOutput: reasoning,
    };
    const visibleTokens = TOKEN_SEGMENTS.reduce(
      (sum, segment) => (hidden.has(segment.key) ? sum : sum + values[segment.key]),
      0,
    );
    return {
      key: rowKey(row),
      id: row.id,
      tsMs: row.ts_ms,
      clock: traceClock(row.ts_ms),
      prompt,
      cached,
      uncachedInput,
      completion,
      reasoning,
      visibleOutput,
      totalTokens: prompt + completion,
      cachePct: cacheReusePct(cached, prompt),
      hasUsage: usage !== null,
      visibleTokens,
      heightFraction: 0,
    };
  });
  const maxVisibleTokens = columns.reduce((max, column) => Math.max(max, column.visibleTokens), 0);
  for (const column of columns) {
    column.heightFraction = maxVisibleTokens > 0 ? column.visibleTokens / maxVisibleTokens : 0;
  }
  return {
    columns,
    summary: {
      calls,
      missingUsage,
      promptTotal,
      cachedTotal,
      completionTotal,
      reasoningTotal,
      cacheReusePct: cacheReusePct(cachedTotal, promptTotal),
    },
    maxVisibleTokens,
    hasData: columns.some((column) => column.visibleTokens > 0),
  };
}

// ---------------------------------------------------------------------------
// Cost over time — auto buckets, stacked by agent or model
// ---------------------------------------------------------------------------

const MINUTE_MS = 60_000;
const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;

/** The unit a cost chart buckets by, chosen from the filtered time span. */
export type CostBucketUnit = "minute" | "hour" | "day";

/** What the cost stack groups by: who made the call, or which model answered. */
export type CostGrouping = "agent" | "model";

/**
 * Pick the bucket unit for a time span: minute buckets up to a two-hour span,
 * hour buckets up to three days, day buckets beyond. Keeps the chart at a
 * readable number of columns for the session-scale filters the Trace page uses.
 */
export function costBucketUnit(spanMs: number): CostBucketUnit {
  const span = Number.isFinite(spanMs) ? Math.max(0, spanMs) : 0;
  if (span <= 2 * HOUR_MS) return "minute";
  if (span <= 3 * DAY_MS) return "hour";
  return "day";
}

/** The width of one bucket in milliseconds. */
export function costBucketWidth(unit: CostBucketUnit): number {
  if (unit === "minute") return MINUTE_MS;
  if (unit === "hour") return HOUR_MS;
  return DAY_MS;
}

/** The start of the UTC-aligned bucket that contains `tsMs`. */
export function costBucketStart(tsMs: number, unit: CostBucketUnit): number {
  const width = costBucketWidth(unit);
  return Math.floor(tsMs / width) * width;
}

const CLOCK_FORMAT = new Intl.DateTimeFormat("en-US", {
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
  timeZone: "UTC",
});
const STAMP_FORMAT = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
  timeZone: "UTC",
});

/** The short axis label of a bucket: `14:05`, `14:00`, or `Apr 11`. */
export function costBucketLabel(startMs: number, unit: CostBucketUnit): string {
  if (unit === "day") return dayLabel(Math.floor(startMs / 1000));
  return CLOCK_FORMAT.format(new Date(startMs));
}

/** The full tooltip/table label of a bucket, with its UTC range. */
export function costBucketTitle(
  startMs: number,
  endMs: number,
  unit: CostBucketUnit,
): string {
  if (unit === "day") {
    return `${dayLabel(Math.floor(startMs / 1000))} · UTC day`;
  }
  return `${STAMP_FORMAT.format(new Date(startMs))}–${CLOCK_FORMAT.format(new Date(endMs))} UTC`;
}

/** One series in the cost stack: an agent identity or a model id. */
export interface CostSeries {
  key: string;
  label: string;
}

/** One visible series' contribution to one bucket. */
export interface CostBucketSeries {
  key: string;
  label: string;
  costMicros: number;
}

/** One auto bucket of estimated spend, calls, and tokens. */
export interface CostBucket {
  startMs: number;
  endMs: number;
  /** Short axis label. */
  label: string;
  /** Full UTC range for tooltips and the values table. */
  title: string;
  /** Every call that started in this bucket. */
  calls: number;
  /** `prompt + completion` across those calls. */
  tokens: number;
  /** Estimated spend of the priced calls, micro-USD. */
  costMicros: number;
  /** Calls whose cost is known. */
  pricedCalls: number;
  /** Terminal calls with no price — hatched, never counted as $0. */
  unpricedCalls: number;
  /** Tokens of the unpriced calls. */
  unpricedTokens: number;
  /** Calls still running (not in the estimate). */
  liveCalls: number;
  /** Per-series spend, only series with a positive cost, chart order. */
  seriesCosts: CostBucketSeries[];
  /** Spend of the series that are not hidden. */
  visibleCostMicros: number;
  /** Bar height against the tallest visible bucket (0..1). */
  heightFraction: number;
}

/** Everything the Cost tab renders for one row set. */
export interface CostChart {
  unit: CostBucketUnit;
  buckets: CostBucket[];
  /** The priced series, biggest total first. */
  series: CostSeries[];
  totalCostMicros: number;
  pricedCalls: number;
  unpricedCalls: number;
  unpricedTokens: number;
  liveCalls: number;
  calls: number;
  tokens: number;
  /** The tallest visible bucket (micro-USD). */
  maxVisibleCostMicros: number;
  /** No rows at all. */
  empty: boolean;
  /** Terminal calls exist but none has a price. */
  allUnpriced: boolean;
}

/** The display label of an attribution, or "Unattributed" when absent. */
export function attributionLabel(attribution: TraceAttribution | null | undefined): string {
  if (!attribution) return "Unattributed";
  switch (attribution.actor_kind) {
    case "lead":
      return "Lead";
    case "worker":
      return attribution.agent_name.trim() || attribution.worker_id || "Worker";
    case "subagent":
      return attribution.agent_name.trim() || "Subagent";
    case "system":
      return "System";
    default:
      return "Unattributed";
  }
}

/** A stable series key for an attribution; two workers never share one. */
function agentSeriesKey(attribution: TraceAttribution): string {
  switch (attribution.actor_kind) {
    case "lead":
      return "lead";
    case "worker":
      return `worker:${attribution.worker_id ?? (attribution.agent_id || attribution.agent_name)}`;
    case "subagent":
      return `subagent:${attribution.agent_id || attribution.agent_name}`;
    case "system":
      return "system";
    default:
      return "unattributed";
  }
}

/** The cost series one row belongs to under the chosen grouping. */
function seriesForRow(row: TraceRow, grouping: CostGrouping): CostSeries {
  if (grouping === "model") {
    return {
      key: `model:${row.model}`,
      label: shortModelLabel(row.model),
    };
  }
  const attribution = row.attribution ?? null;
  if (!attribution) return { key: "unattributed", label: "Unattributed" };
  return { key: agentSeriesKey(attribution), label: attributionLabel(attribution) };
}

/** The priced micro-USD of one row, or null when it has no price. */
function pricedCost(row: TraceRow): number | null {
  if (row.priced === false) return null;
  const cost = row.cost_micros;
  if (typeof cost !== "number" || !Number.isFinite(cost) || cost < 0) return null;
  return cost;
}

/** An empty cost chart, used before the first call and by empty filters. */
function emptyCostChart(): CostChart {
  return {
    unit: "minute",
    buckets: [],
    series: [],
    totalCostMicros: 0,
    pricedCalls: 0,
    unpricedCalls: 0,
    unpricedTokens: 0,
    liveCalls: 0,
    calls: 0,
    tokens: 0,
    maxVisibleCostMicros: 0,
    empty: true,
    allUnpriced: false,
  };
}

/**
 * Bucket the filtered rows by time and stack their estimated spend by agent or
 * model.
 *
 * Buckets are UTC-aligned (matching the Cost tab's UTC days) and gaps between
 * calls are filled with empty buckets so the x axis stays a continuous
 * timeline. Only rows with a final price contribute dollars; a terminal row
 * without one lands in the hatched Unpriced count, and a row still running is
 * counted separately because its cost is not known yet. `hidden` holds legend
 * series keys; hidden series drop out of the height driver, not out of the
 * table's values.
 */
export function costChart(
  rows: readonly TraceRow[],
  grouping: CostGrouping,
  hidden: ReadonlySet<string> = new Set(),
): CostChart {
  if (rows.length === 0) return emptyCostChart();
  const sorted = ordered(rows);
  const first = sorted[0].ts_ms;
  const last = sorted[sorted.length - 1].ts_ms;
  const unit = costBucketUnit(last - first);
  const width = costBucketWidth(unit);
  const firstStart = costBucketStart(first, unit);
  const lastStart = costBucketStart(last, unit);
  const byStart = new Map<number, CostBucket>();
  const buckets: CostBucket[] = [];
  for (let start = firstStart; start <= lastStart; start += width) {
    const bucket: CostBucket = {
      startMs: start,
      endMs: start + width,
      label: costBucketLabel(start, unit),
      title: costBucketTitle(start, start + width, unit),
      calls: 0,
      tokens: 0,
      costMicros: 0,
      pricedCalls: 0,
      unpricedCalls: 0,
      unpricedTokens: 0,
      liveCalls: 0,
      seriesCosts: [],
      visibleCostMicros: 0,
      heightFraction: 0,
    };
    byStart.set(start, bucket);
    buckets.push(bucket);
  }

  const totals = new Map<string, { series: CostSeries; costMicros: number }>();
  let pricedCalls = 0;
  let unpricedCalls = 0;
  let unpricedTokens = 0;
  let liveCalls = 0;
  let calls = 0;
  let tokens = 0;
  for (const row of sorted) {
    const bucket = byStart.get(costBucketStart(row.ts_ms, unit));
    if (!bucket) continue;
    const rowTotal = rowTokens(row);
    bucket.calls += 1;
    bucket.tokens += rowTotal;
    calls += 1;
    tokens += rowTotal;
    const cost = pricedCost(row);
    if (cost === null) {
      if (row.is_complete) {
        bucket.unpricedCalls += 1;
        bucket.unpricedTokens += rowTotal;
        unpricedCalls += 1;
        unpricedTokens += rowTotal;
      } else {
        bucket.liveCalls += 1;
        liveCalls += 1;
      }
      continue;
    }
    const series = seriesForRow(row, grouping);
    bucket.pricedCalls += 1;
    bucket.costMicros += cost;
    pricedCalls += 1;
    const total = totals.get(series.key);
    if (total) total.costMicros += cost;
    else totals.set(series.key, { series, costMicros: cost });
    const slice = bucket.seriesCosts.find((entry) => entry.key === series.key);
    if (slice) slice.costMicros += cost;
    else bucket.seriesCosts.push({ key: series.key, label: series.label, costMicros: cost });
  }

  const series = [...totals.values()]
    .sort(
      (a, b) =>
        b.costMicros - a.costMicros ||
        a.series.label.localeCompare(b.series.label) ||
        a.series.key.localeCompare(b.series.key),
    )
    .map((entry) => entry.series);
  const seriesOrder = new Map(series.map((entry, index) => [entry.key, index]));
  for (const bucket of buckets) {
    bucket.seriesCosts.sort(
      (a, b) => (seriesOrder.get(a.key) ?? 0) - (seriesOrder.get(b.key) ?? 0),
    );
  }

  let maxVisibleCostMicros = 0;
  for (const bucket of buckets) {
    bucket.visibleCostMicros = bucket.seriesCosts.reduce(
      (sum, slice) => (hidden.has(slice.key) ? sum : sum + slice.costMicros),
      0,
    );
    maxVisibleCostMicros = Math.max(maxVisibleCostMicros, bucket.visibleCostMicros);
  }
  for (const bucket of buckets) {
    bucket.heightFraction =
      maxVisibleCostMicros > 0 ? bucket.visibleCostMicros / maxVisibleCostMicros : 0;
  }

  return {
    unit,
    buckets,
    series,
    totalCostMicros: buckets.reduce((sum, bucket) => sum + bucket.costMicros, 0),
    pricedCalls,
    unpricedCalls,
    unpricedTokens,
    liveCalls,
    calls,
    tokens,
    maxVisibleCostMicros,
    empty: false,
    allUnpriced: unpricedCalls > 0 && pricedCalls === 0,
  };
}

/** A bucket's estimated dollars, or "—" when nothing in it had a price. */
export function bucketCostLabel(bucket: CostBucket): string {
  return bucket.pricedCalls > 0 ? formatEstimate(bucket.costMicros) : "—";
}
