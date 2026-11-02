/**
 * The Cost tab's data layer.
 *
 * The transport calls the two `insights_*` Tauri commands from
 * `src-tauri/src/mnemo2/insights.rs`; the wire is plain serde, so the field
 * names below mirror the Rust structs exactly (snake_case). `costViewModel`
 * turns the two reports into everything `CostView.svelte` renders, and the
 * lever table maps the savings ledger's raw kinds to friendly names.
 */

import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";
import { formatTokenCount, formatUsageCost } from "./usage";

// ---------------------------------------------------------------------------
// Wire types — see src-tauri/src/mnemo2/insights.rs (serde keeps snake_case).
// ---------------------------------------------------------------------------

/** One model's tokens and estimated spend (`ModelCost`). */
export interface ModelCost {
  /** The observed model id as recorded in the ledger. */
  model: string;
  /** Uncached input tokens (prompt tokens minus cached tokens). */
  input_tokens: number;
  /** Cached prompt tokens. */
  cached_tokens: number;
  /** Completion (output) tokens. */
  output_tokens: number;
  /** Estimated spend in micro-USD, or null when the model has no price. */
  cost_micros: number | null;
}

/** Project-wide token totals and priced spend (`CostTotal`). */
export interface CostTotal {
  input_tokens: number;
  cached_tokens: number;
  output_tokens: number;
  /** Estimated cost in micro-USD for the priced usage only. */
  cost_micros: number;
}

/** One day of estimated spend (`CostDay`). */
export interface CostDay {
  /** Start of the UTC day, in Unix seconds. */
  day: number;
  cost_micros: number;
}

/** The `insights_cost` report (`CostReport`). */
export interface CostReport {
  total: CostTotal;
  /** Per-model rows; priced first (largest cost first), unpriced last. */
  models: ModelCost[];
  /** Models with recorded usage but no price, sorted by model id. */
  unpriced_models: string[];
  /** Estimated spend for the last 14 UTC days; empty when nothing is priced. */
  per_day: CostDay[];
}

/** One savings lever's tokens and estimated dollars (`SavingsLever`). */
export interface SavingsLever {
  /** The ledger's lever kind (mapped to a friendly name below). */
  kind: string;
  /** Number of ledger events of this kind. */
  events: number;
  /** Tokens saved (signed; re-expansions subtract). */
  tokens_saved: number;
  /** Estimated dollars saved in micro-USD, or null when no price is known. */
  estimated_saved_micros: number | null;
}

/** Prompt-cache totals and what the hits saved (`SavingsCache`). */
export interface SavingsCache {
  prompt_tokens: number;
  cached_tokens: number;
  /** `cached_tokens / prompt_tokens`; `0.0` when nothing was requested. */
  hit_rate: number;
  estimated_saved_micros: number | null;
}

/** One day of tokens saved (`SavingsDay`). */
export interface SavingsDay {
  /** Start of the UTC day, in Unix seconds. */
  day: number;
  /** Tokens saved that day (signed). */
  tokens_saved: number;
}

/** The `insights_savings` report (`SavingsReport`). */
export interface SavingsReport {
  tokens_saved: number;
  estimated_saved_micros: number | null;
  /** Per-lever rows, largest saving first (the ledger's order). */
  levers: SavingsLever[];
  cache: SavingsCache;
  /** Tokens saved for the last 14 UTC days. */
  per_day: SavingsDay[];
}

/** Both reports the tab needs — fetched together, rendered together. */
export interface CostInsights {
  cost: CostReport;
  savings: SavingsReport;
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/** The Tauri calls the transport needs; injectable for tests. */
export interface CostTransportBindings {
  invoke: typeof invoke;
}

/** What CostView needs from the host: the two `insights_*` reports. */
export interface CostTransport {
  cost(): Promise<CostReport>;
  savings(): Promise<SavingsReport>;
}

/** Transport bound to Tauri's `invoke` (the real commands take no arguments). */
export function createTauriCostTransport(bindings: CostTransportBindings): CostTransport {
  return {
    cost: () => bindings.invoke<CostReport>("insights_cost"),
    savings: () => bindings.invoke<SavingsReport>("insights_savings"),
  };
}

/**
 * The Cost tab's transport, or null outside the Mnemo2 host: a plain browser
 * page has no Tauri IPC (the same shape SettingsDialog uses).
 */
export function createCostTransport(bindings?: CostTransportBindings): CostTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriCostTransport(bindings ?? { invoke });
}

/**
 * Dev-only preview seam: the preview page (`preview.html?scene=cost`) runs in
 * a browser with no Tauri host, so its `cost` scene seeds the reports the
 * transport would have fetched. Only `src/test/previewScenes.ts` ever calls
 * the setter — the real app has a transport and never reads the seed.
 */
let previewInsights: CostInsights | null = null;

/** Seed (or clear) the reports the browser preview renders. */
export function seedCostPreview(insights: CostInsights | null): void {
  previewInsights = insights;
}

/** The seeded reports, or null when no scene has seeded any. */
export function costPreviewInsights(): CostInsights | null {
  return previewInsights;
}

// ---------------------------------------------------------------------------
// Savings levers — friendly names for the ledger's kinds
// ---------------------------------------------------------------------------

/** Each lever's display name and a one-line explanation of what it does. */
const LEVERS: Record<string, { label: string; explanation: string }> = {
  delta_read: {
    label: "Delta reads",
    explanation: "A file that changed since its last read is served as a diff, not as the whole file.",
  },
  skeleton: {
    label: "Signature skeletons",
    explanation: "An unchanged re-read serves the file's symbol signatures instead of its full text.",
  },
  outline: {
    label: "Outlines",
    explanation: "A long file's first read opens with its outline; the rest loads on demand.",
  },
  compression: {
    label: "Output compression",
    explanation: "Command output is compressed before it enters the context.",
  },
  truncation: {
    label: "Output caps",
    explanation: "Tool output past the size cap is cut instead of kept in full.",
  },
  archive: {
    label: "Archived results",
    explanation: "Oversized tool results are archived in full; only a short preview stays in context.",
  },
  archive_expand: {
    label: "Archive expansions",
    explanation: "Expanding an archived result re-adds its tokens, recorded as negative savings.",
  },
  recall_delta: {
    label: "Compact recalls",
    explanation: "A repeated memory recall serves a short reference instead of the full snippet.",
  },
  compaction_checkpoint: {
    label: "Compaction checkpoints",
    explanation: "The context compaction drops is checkpointed first; the row records its size, not a saving.",
  },
};

/** Prettify an unknown kind so a new lever never renders as its raw id. */
function fallbackLabel(kind: string): string {
  const words = kind.split(/[_-]+/).filter((word) => word.length > 0);
  if (words.length === 0) return "Other levers";
  // Sentence case, matching the known labels ("Delta reads", not "Delta Reads").
  return words
    .map((word, index) => (index === 0 ? `${word[0]!.toUpperCase()}${word.slice(1)}` : word))
    .join(" ");
}

/** The friendly name of a savings lever kind. */
export function leverLabel(kind: string): string {
  return LEVERS[kind]?.label ?? fallbackLabel(kind);
}

/** A one-line explanation of what the lever does. */
export function leverExplanation(kind: string): string {
  return LEVERS[kind]?.explanation ?? "An optimization lever the engine recorded.";
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/** The short display name of a model id: the segment after the last "/". */
export function shortModelLabel(model: string): string {
  const segment = (model.split("/").pop() ?? model).trim();
  return segment === "" ? model : segment;
}

/** Token counts that keep their sign: an expansion costs tokens and shows a minus. */
export function formatSignedTokens(tokens: number): string {
  const count = Number.isFinite(tokens) ? Math.trunc(tokens) : 0;
  return count.toLocaleString("en-US");
}

/** Estimated dollars: "≈ $1.23", or "—" when no price is known. */
export function formatEstimate(costMicros: number | null): string {
  if (costMicros === null || !Number.isFinite(costMicros)) return "—";
  return `≈ ${formatUsageCost(costMicros)}`;
}

/** A hit rate (0..1) as a whole percentage, e.g. "77%". */
export function formatHitRate(rate: number): string {
  const fraction = Number.isFinite(rate) ? Math.min(Math.max(rate, 0), 1) : 0;
  return `${Math.round(fraction * 100)}%`;
}

/** The engine buckets days in UTC, so the labels format in UTC to match. */
const DAY_FORMAT = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", timeZone: "UTC" });
const MONTH_FORMAT = new Intl.DateTimeFormat("en-US", { month: "short", timeZone: "UTC" });
const DAY_NUMBER = new Intl.DateTimeFormat("en-US", { day: "numeric", timeZone: "UTC" });

/** "Apr 11" for a UTC-day bucket (epoch seconds). */
export function dayLabel(day: number): string {
  return DAY_FORMAT.format(new Date(day * 1000));
}

/** The month of a UTC-day bucket, so a month change can widen the label. */
function monthLabel(day: number): string {
  return MONTH_FORMAT.format(new Date(day * 1000));
}

/** The day of the month alone ("11"), for same-month columns. */
function dayNumberLabel(day: number): string {
  return DAY_NUMBER.format(new Date(day * 1000));
}

// ---------------------------------------------------------------------------
// View model
// ---------------------------------------------------------------------------

/** One "spend by model" row. */
export interface CostModelRow {
  /** The model id exactly as the ledger recorded it (the React key). */
  id: string;
  /** The short display name. */
  label: string;
  /** The model's total tokens, formatted. */
  tokensLabel: string;
  /** The row tooltip: the in / cached / out split behind the total. */
  detailLabel: string;
  /** Bar width: the model's share of the heaviest model's tokens (0..1). */
  barFraction: number;
  /** The estimated spend, or null when the model has no price. */
  costMicros: number | null;
  /** The dollars cell: "$2.00" or "no price". */
  costLabel: string;
}

/** One savings lever row, with the friendly name and explanation attached. */
export interface CostLeverRow {
  /** The ledger kind (the React key). */
  kind: string;
  /** The friendly lever name. */
  label: string;
  /** A one-line explanation of the lever. */
  explanation: string;
  /** The whole meta cell: "1,200,000 tokens · 40 events", or
   * "+120,000 tokens re-added · 6 events" for a cost row. */
  metaLabel: string;
  /** The estimated dollars cell: "≈ $0.18" or "—" without a price. A lever
   * that re-added tokens is a cost, not a saving: it shows no dollars at all. */
  costLabel: string;
  /** Whether the lever re-added tokens (a cost, not a saving). */
  readded: boolean;
}

/** One merged day column shared by both bar charts. */
export interface CostDayRow {
  /** The UTC-day bucket (epoch seconds). */
  day: number;
  /** "Apr 11" — the full label for titles. */
  label: string;
  /** Wider when the month changes, the day number otherwise ("11"). */
  shortLabel: string;
  /** Estimated spend that day, in micro-USD. */
  costMicros: number;
  costLabel: string;
  /** Bar height against the series' peak (0..1). */
  costFraction: number;
  /** Tokens saved that day (signed). */
  tokensSaved: number;
  tokensSavedLabel: string;
  /** Bar height against the savings series' peak (0..1). */
  tokensFraction: number;
}

/** Everything CostView renders, derived from the two reports. */
export interface CostViewModel {
  /** Nothing has run yet — the tab shows its empty state. */
  empty: boolean;
  /** Some recorded usage has no configured price. */
  partial: boolean;
  /** The unpriced model names for the partial note. */
  unpricedLabel: string;
  /** Headline: total estimated spend. */
  spendLabel: string;
  /** Headline: "1,200,000 in · 800,000 cached · 90,000 out". */
  tokensLabel: string;
  /** Spend by model, most expensive first, unpriced last. */
  models: CostModelRow[];
  /** Total tokens saved, signed and formatted. */
  savingsTokensLabel: string;
  /** The estimated total saved: "≈ $0.42" or "—". */
  savingsCostLabel: string;
  /** One row per lever, biggest saver first. */
  levers: CostLeverRow[];
  /** Whether any prompt tokens were recorded. */
  cacheActive: boolean;
  /** "77%". */
  cacheHitRateLabel: string;
  /** "1,540,000 of 2,000,000 prompt tokens served from cache". */
  cacheDetailLabel: string;
  /** The cache's estimated dollars saved. */
  cacheSavedLabel: string;
  /** The merged day series, ascending (at most the last 14 days). */
  days: CostDayRow[];
  /** Whether any day carries spend. */
  daysHaveSpend: boolean;
  /** Whether any day carries savings. */
  daysHaveSavings: boolean;
}

/** Total tokens one model consumed: uncached input + cached + output. */
function modelTokens(row: ModelCost): number {
  return row.input_tokens + row.cached_tokens + row.output_tokens;
}

/** The "spend by model" rows: cost descends, unpriced models sink to the end. */
function modelRows(models: ModelCost[]): CostModelRow[] {
  const maxTokens = models.reduce((max, row) => Math.max(max, modelTokens(row)), 0);
  return [...models]
    .sort(
      (a, b) =>
        (b.cost_micros ?? -1) - (a.cost_micros ?? -1) ||
        modelTokens(b) - modelTokens(a),
    )
    .map((row) => ({
      id: row.model,
      label: shortModelLabel(row.model),
      tokensLabel: formatTokenCount(modelTokens(row)),
      detailLabel: `${formatTokenCount(row.input_tokens)} in · ${formatTokenCount(row.cached_tokens)} cached · ${formatTokenCount(row.output_tokens)} out`,
      barFraction: maxTokens === 0 ? 0 : modelTokens(row) / maxTokens,
      costMicros: row.cost_micros,
      costLabel: row.cost_micros === null ? "no price" : formatUsageCost(row.cost_micros),
    }));
}

/** The lever rows with friendly names, biggest saver first. */
function leverRows(levers: SavingsLever[]): CostLeverRow[] {
  return [...levers]
    .sort((a, b) => b.tokens_saved - a.tokens_saved)
    .map((lever) => {
      const eventsLabel = `${formatTokenCount(lever.events)} ${lever.events === 1 ? "event" : "events"}`;
      // A net-negative lever (an expansion re-adding tokens) is a cost, not a
      // saving: the backend prices it at $0 (insights.rs clamps at
      // `tokens.max(0)`), so the row shows the tokens it re-added and no dollar
      // figure — dollars next to a minus would read as a saving either way.
      const readded = lever.tokens_saved < 0;
      return {
        kind: lever.kind,
        label: leverLabel(lever.kind),
        explanation: leverExplanation(lever.kind),
        metaLabel: readded
          ? `+${formatSignedTokens(-lever.tokens_saved)} tokens re-added · ${eventsLabel}`
          : `${formatSignedTokens(lever.tokens_saved)} tokens · ${eventsLabel}`,
        costLabel: readded ? "" : formatEstimate(lever.estimated_saved_micros),
        readded,
      };
    });
}

/** Merge the spend and savings day series into one aligned column set. */
function dayRows(costDays: CostDay[], savingsDays: SavingsDay[]): CostDayRow[] {
  const merged = new Map<number, { cost: number; saved: number }>();
  for (const day of costDays) merged.set(day.day, { cost: day.cost_micros, saved: 0 });
  for (const day of savingsDays) {
    const row = merged.get(day.day) ?? { cost: 0, saved: 0 };
    row.saved = day.tokens_saved;
    merged.set(day.day, row);
  }
  const entries = [...merged.entries()].sort((a, b) => a[0] - b[0]).slice(-14);
  const maxCost = entries.reduce((max, [, row]) => Math.max(max, row.cost), 0);
  const maxSaved = entries.reduce((max, [, row]) => Math.max(max, Math.max(0, row.saved)), 0);
  return entries.map(([day, row], index) => {
    const label = dayLabel(day);
    const month = monthLabel(day);
    const previousMonth = index > 0 ? monthLabel(entries[index - 1]![0]) : null;
    return {
      day,
      label,
      shortLabel: index === 0 || month !== previousMonth ? label : dayNumberLabel(day),
      costMicros: row.cost,
      costLabel: formatUsageCost(row.cost),
      costFraction: maxCost === 0 ? 0 : row.cost / maxCost,
      tokensSaved: row.saved,
      tokensSavedLabel: formatSignedTokens(row.saved),
      tokensFraction: maxSaved === 0 ? 0 : Math.max(0, row.saved) / maxSaved,
    };
  });
}

/** Derive everything the Cost tab renders from the two reports. */
export function costViewModel(report: CostReport, savings: SavingsReport): CostViewModel {
  const totalTokens = report.total.input_tokens + report.total.cached_tokens + report.total.output_tokens;
  const days = dayRows(report.per_day, savings.per_day);
  return {
    empty:
      totalTokens === 0 &&
      savings.tokens_saved === 0 &&
      savings.levers.length === 0 &&
      savings.cache.prompt_tokens === 0,
    partial: report.unpriced_models.length > 0,
    unpricedLabel: report.unpriced_models.map(shortModelLabel).join(", "),
    spendLabel: formatUsageCost(report.total.cost_micros),
    tokensLabel: `${formatTokenCount(report.total.input_tokens)} in · ${formatTokenCount(report.total.cached_tokens)} cached · ${formatTokenCount(report.total.output_tokens)} out`,
    models: modelRows(report.models),
    savingsTokensLabel: formatSignedTokens(savings.tokens_saved),
    savingsCostLabel: formatEstimate(savings.estimated_saved_micros),
    levers: leverRows(savings.levers),
    cacheActive: savings.cache.prompt_tokens > 0,
    cacheHitRateLabel: formatHitRate(savings.cache.hit_rate),
    cacheDetailLabel: `${formatTokenCount(savings.cache.cached_tokens)} of ${formatTokenCount(savings.cache.prompt_tokens)} prompt tokens served from cache`,
    cacheSavedLabel: formatEstimate(savings.cache.estimated_saved_micros),
    days,
    daysHaveSpend: days.some((day) => day.costMicros > 0),
    daysHaveSavings: days.some((day) => day.tokensSaved !== 0),
  };
}