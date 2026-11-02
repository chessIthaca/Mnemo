import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";
import type { EndpointDetailDto } from "./settingsEngine";

/**
 * Model-role settings for the Models and Savings pages: the transport to
 * `model_settings_get` / `model_settings_save` / `embedding_install`, the
 * view-model metadata (task slots, lever names and explanations), and the
 * draft parsing the pages save with. The wire shapes mirror
 * `ModelSettingsWire` in `src-tauri/src/mnemo2/model_settings.rs` — the
 * JSON keys are snake_case, like every other Mnemo2 DTO.
 */

/** The channel the embedder install streams `SetupEvent` payloads on. */
export const EMBEDDER_EVENT_CHANNEL = "mnemo2://embedder-event";

/** The config id of the keyword-only sentinel ("no embedding model"). */
export const EMBEDDING_KEYWORD_ID = "hash";

// ── Wire shapes (the `model_settings_get` snapshot) ──────────────���───────────

/** The five per-task `[models]` overrides, each `"endpoint/model"` or null. */
export interface TaskModels {
  planning: string | null;
  executing: string | null;
  bug_fixing: string | null;
  reviewing: string | null;
  summarize: string | null;
}

/** One selectable code-indexing model. */
export interface EmbeddingOption {
  id: string;
  label: string;
  size_mb: number;
  installed: boolean;
}

/** Where the code-indexing embedder stands right now. */
export type EmbeddingStatus = "ready" | "missing" | "downloading" | "failed" | "keyword_only";

/** The code-indexing section: selection, catalog and live status. */
export interface EmbeddingSection {
  selected: string;
  options: EmbeddingOption[];
  status: EmbeddingStatus;
}

/** The token-optimizer levers (switches) and knobs (numbers). */
export interface OptimizerSettings {
  delta_reads: boolean;
  compress_output: boolean;
  archive: boolean;
  compaction_survival: boolean;
  quality_score: boolean;
  lean_output_nudge: boolean;
  recall_delta: boolean;
  archive_min_chars: number;
  compress_min_chars: number;
  lean_output_fill_pct: number;
  nudge_cooldown_requests: number;
  compress_extra_commands: string[];
}

/** One per-model pricing row, US dollars per 1M tokens. */
export interface PricingRow {
  model: string;
  input_per_1m: number;
  output_per_1m: number;
  cached_per_1m: number;
}

/** The deterministic budget caps. */
export interface BudgetCaps {
  enabled: boolean;
  max_tokens_per_plan: number;
  max_escalations_per_plan: number;
  max_retries_per_lane: number;
}

/** The full `model_settings_get` response. */
export interface ModelSettings {
  task_models: TaskModels;
  vision_model: string | null;
  embedding: EmbeddingSection;
  optimizer: OptimizerSettings;
  pricing: PricingRow[];
  budget: BudgetCaps;
}

/** One page-owned section a save writes in isolation. */
export type ModelSettingsSection = "models" | "vision" | "embedding" | "optimizer" | "pricing" | "budget";

/** A `SetupEvent` payload from `embedding_install`: progress, then done/failed. */
export interface EmbedderEvent {
  type: "progress" | "done" | "failed";
  step: string;
  label?: string;
  fraction?: number | null;
  error?: string;
}

// ── View-model metadata ──────────────────────────────────────────────────────

/** One per-task model slot the Models page edits. */
export interface TaskSlot {
  key: keyof TaskModels;
  label: string;
  blurb: string;
}

/** The five task slots, in display order. */
export const TASK_SLOTS: readonly TaskSlot[] = [
  { key: "planning", label: "Planning", blurb: "Writes the plan before any work starts." },
  { key: "executing", label: "Executing", blurb: "Carries out the steps of a plan." },
  { key: "bug_fixing", label: "Bug fixing", blurb: "The lane that fixes a failed check." },
  { key: "reviewing", label: "Reviewing", blurb: "The reviewer that inspects a worker's branch." },
  { key: "summarize", label: "Summarize", blurb: "Compacts the conversation when the context fills." },
];

/** One optimizer lever: a switch with a one-line explanation. */
export interface OptimizerLever {
  key: Exclude<keyof OptimizerSettings, keyof OptimizerKnobs>;
  label: string;
  explanation: string;
}

/** The optimizer knobs — small inputs beside the levers, drafted as text. */
export interface OptimizerKnobs {
  archive_min_chars: string;
  compress_min_chars: string;
  lean_output_fill_pct: string;
  nudge_cooldown_requests: string;
  compress_extra_commands: string;
}

/** The levers the Savings page switches, in display order. */
export const OPTIMIZER_LEVERS: readonly OptimizerLever[] = [
  { key: "delta_reads", label: "Delta reads", explanation: "Re-read file signatures and diffs instead of full file contents." },
  { key: "compress_output", label: "Compress shell output", explanation: "Squeeze long command output down before it enters the context." },
  { key: "archive", label: "Archive big results", explanation: "Fold oversized tool results into archives and expand them on demand." },
  { key: "compaction_survival", label: "Compaction survival", explanation: "Keep the checkpoint, decisions and digest through compaction." },
  { key: "quality_score", label: "Context quality score", explanation: "Track the S-F context-quality score on usage events." },
  { key: "lean_output_nudge", label: "Lean-output nudge", explanation: "Nudge for shorter answers while the context is nearly full." },
  { key: "recall_delta", label: "Recall deltas", explanation: "Re-inject only the changed part of a recalled memory." },
];

/** One optimizer knob: a small number input with its own explanation. */
export interface OptimizerKnob {
  key: keyof OptimizerKnobs;
  label: string;
  explanation: string;
  /** Text inputs; `false` marks the one free-text knob (command patterns). */
  isNumber: boolean;
}

/** The knobs the Savings page edits, in display order. */
export const OPTIMIZER_KNOBS: readonly OptimizerKnob[] = [
  { key: "archive_min_chars", label: "Archive after", explanation: "Tool results longer than this many characters are archived.", isNumber: true },
  { key: "compress_min_chars", label: "Compress after", explanation: "Filtered output longer than this many characters is compressed.", isNumber: true },
  { key: "lean_output_fill_pct", label: "Nudge at fill %", explanation: "Context fill that triggers the lean-output nudge (0–100).", isNumber: true },
  { key: "nudge_cooldown_requests", label: "Nudge every", explanation: "Requests between two lean-output nudges.", isNumber: true },
  { key: "compress_extra_commands", label: "Extra commands", explanation: "User-added command patterns eligible for compression, comma-separated.", isNumber: false },
];

/** The status line wording for each embedder status. */
export const EMBEDDING_STATUS_META: Record<EmbeddingStatus, string> = {
  ready: "Ready",
  missing: "Not installed",
  downloading: "Downloading…",
  failed: "Failed",
  keyword_only: "Keyword only",
};

// ── Drafts and parsing ───────────────────────────────────────────────────────

/** An editable optimizer draft: lever switches plus knob inputs as text. */
export type OptimizerDraft = { [L in OptimizerLever["key"]]: boolean } & OptimizerKnobs;

/** Seed an optimizer draft from the wire settings. */
export function optimizerDraft(settings: OptimizerSettings): OptimizerDraft {
  return {
    delta_reads: settings.delta_reads,
    compress_output: settings.compress_output,
    archive: settings.archive,
    compaction_survival: settings.compaction_survival,
    quality_score: settings.quality_score,
    lean_output_nudge: settings.lean_output_nudge,
    recall_delta: settings.recall_delta,
    archive_min_chars: String(settings.archive_min_chars),
    compress_min_chars: String(settings.compress_min_chars),
    lean_output_fill_pct: String(settings.lean_output_fill_pct),
    nudge_cooldown_requests: String(settings.nudge_cooldown_requests),
    compress_extra_commands: settings.compress_extra_commands.join(", "),
  };
}

/** One editable pricing row: the numbers as input text. */
export interface PricingDraftRow {
  model: string;
  input: string;
  output: string;
  cached: string;
}

/** Seed pricing draft rows from the wire table. */
export function pricingDraft(rows: PricingRow[]): PricingDraftRow[] {
  return rows.map((row) => ({
    model: row.model,
    input: String(row.input_per_1m),
    output: String(row.output_per_1m),
    cached: String(row.cached_per_1m),
  }));
}

/** An editable budget draft: the caps as input text. */
export interface BudgetDraft {
  enabled: boolean;
  max_tokens_per_plan: string;
  max_escalations_per_plan: string;
  max_retries_per_lane: string;
}

/** Seed a budget draft from the wire caps. */
export function budgetDraft(caps: BudgetCaps): BudgetDraft {
  return {
    enabled: caps.enabled,
    max_tokens_per_plan: String(caps.max_tokens_per_plan),
    max_escalations_per_plan: String(caps.max_escalations_per_plan),
    max_retries_per_lane: String(caps.max_retries_per_lane),
  };
}

/**
 * A compact model reference (`"endpoint/model"`): split at the first slash,
 * both sides non-empty. Model ids like `org/model-name` survive intact.
 */
export function isValidModelRef(text: string): boolean {
  const value = text.trim();
  const slash = value.indexOf("/");
  if (slash <= 0 || slash === value.length - 1) return false;
  return true;
}

/** Parse a whole non-negative number input; null when it is not one. */
export function parseWholeNumber(text: string): number | null {
  const trimmed = text.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const parsed = Number(trimmed);
  return Number.isSafeInteger(parsed) ? parsed : null;
}

/** Parse a non-negative dollar amount; null when it is not one. Blank is 0. */
export function parsePrice(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === "") return 0;
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return null;
  return Number(trimmed);
}

/** Parse an optimizer draft into the save payload, or the first error. */
export function optimizerWire(draft: OptimizerDraft): { value?: Partial<OptimizerSettings>; error?: string } {
  const value: Partial<OptimizerSettings> = {
    delta_reads: draft.delta_reads,
    compress_output: draft.compress_output,
    archive: draft.archive,
    compaction_survival: draft.compaction_survival,
    quality_score: draft.quality_score,
    lean_output_nudge: draft.lean_output_nudge,
    recall_delta: draft.recall_delta,
  };
  type KnobKey = "archive_min_chars" | "compress_min_chars" | "lean_output_fill_pct" | "nudge_cooldown_requests";
  const numbers: [KnobKey, string, string, (n: number) => boolean][] = [
    ["archive_min_chars", draft.archive_min_chars, "Archive after", (n) => n >= 0],
    ["compress_min_chars", draft.compress_min_chars, "Compress after", (n) => n >= 0],
    ["lean_output_fill_pct", draft.lean_output_fill_pct, "Nudge at fill %", (n) => n >= 0 && n <= 100],
    ["nudge_cooldown_requests", draft.nudge_cooldown_requests, "Nudge every", (n) => n >= 0],
  ];
  const knobs = {} as Record<KnobKey, number>;
  for (const [key, text, label, range] of numbers) {
    const parsed = parseWholeNumber(text);
    if (parsed === null || !range(parsed)) {
      const suffix = key === "lean_output_fill_pct" ? " must be a whole number from 0 to 100" : " must be a whole number";
      return { error: `${label}${suffix}` };
    }
    knobs[key] = parsed;
  }
  value.compress_extra_commands = draft.compress_extra_commands
    .split(",")
    .map((command) => command.trim())
    .filter((command) => command.length > 0);
  return { value: { ...value, ...knobs } };
}

/** Parse pricing draft rows into the save table, or the first error. */
export function pricingWire(draft: PricingDraftRow[]): { value?: PricingRow[]; error?: string } {
  const value: PricingRow[] = [];
  for (const row of draft) {
    const model = row.model.trim();
    if (model === "") continue;
    const prices: [keyof Omit<PricingRow, "model">, string, string][] = [
      ["input_per_1m", row.input, "Input price"],
      ["output_per_1m", row.output, "Output price"],
      ["cached_per_1m", row.cached, "Cached price"],
    ];
    const next: PricingRow = { model, input_per_1m: 0, output_per_1m: 0, cached_per_1m: 0 };
    for (const [key, text, label] of prices) {
      const parsed = parsePrice(text);
      if (parsed === null) return { error: `${label} for ${model} must be a number` };
      next[key] = parsed;
    }
    value.push(next);
  }
  return { value };
}

/** Parse a budget draft into the save caps, or the first error. */
export function budgetWire(draft: BudgetDraft): { value?: BudgetCaps; error?: string } {
  const caps: [keyof Omit<BudgetCaps, "enabled">, string, string][] = [
    ["max_tokens_per_plan", draft.max_tokens_per_plan, "Tokens per plan"],
    ["max_escalations_per_plan", draft.max_escalations_per_plan, "Escalations per plan"],
    ["max_retries_per_lane", draft.max_retries_per_lane, "Retries per lane"],
  ];
  const value: BudgetCaps = {
    enabled: draft.enabled,
    max_tokens_per_plan: 0,
    max_escalations_per_plan: 0,
    max_retries_per_lane: 0,
  };
  for (const [key, text, label] of caps) {
    const parsed = parseWholeNumber(text);
    if (parsed === null) return { error: `${label} must be a whole number` };
    value[key] = parsed;
  }
  return { value };
}

// ── Picker options ───────────────────────────────────────────────────────────

/**
 * The model-picker datalist options: every configured endpoint's models as
 * `"endpoint/model"`, deduplicated in endpoint order. Free text stays
 * possible — the list is an offer, not a gate.
 */
export function modelRefOptions(endpoints: EndpointDetailDto[]): string[] {
  const seen = new Set<string>();
  for (const endpoint of endpoints) {
    for (const model of endpoint.models) {
      seen.add(`${endpoint.name}/${model.id}`);
    }
  }
  return [...seen];
}

/** Model ids (no endpoint prefix) for the pricing table's model column. */
export function modelIdOptions(endpoints: EndpointDetailDto[]): string[] {
  const seen = new Set<string>();
  for (const endpoint of endpoints) {
    for (const model of endpoint.models) {
      seen.add(model.id);
    }
  }
  return [...seen];
}

// ── Transport ────────────────────────────────────────────────────────────────

export interface ModelSettingsTransportBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

/** The IPC surface the Models and Savings pages need. */
export interface ModelSettingsTransport {
  /** The full snapshot; sections the page does not save stay untouched. */
  get(): Promise<ModelSettings>;
  /** Write one section, keeping every other key. */
  save(section: ModelSettingsSection, value: unknown): Promise<void>;
  /** Start a background download of a bundled model (progress via events). */
  install(modelId: string): Promise<void>;
  /** Subscribe to install progress; resolves to the unsubscribe function. */
  onEmbedderEvent(handler: (event: EmbedderEvent) => void): Promise<UnlistenFn>;
}

export function createTauriModelSettingsTransport(bindings: ModelSettingsTransportBindings): ModelSettingsTransport {
  return {
    get: () => bindings.invoke<ModelSettings>("model_settings_get"),
    save: (section, value) => bindings.invoke<void>("model_settings_save", { section, value }),
    install: (modelId) => bindings.invoke<void>("embedding_install", { modelId }),
    onEmbedderEvent: (handler) =>
      bindings.listen<EmbedderEvent>(EMBEDDER_EVENT_CHANNEL, (event) => handler(event.payload)),
  };
}

/**
 * Dev-only seam: the preview page (no Tauri host) seeds a fake transport
 * here before mounting the settings dialog, so the pages render with sample
 * data in a browser. Nothing in the shipped app calls the setter, and the
 * browser check below still returns null when no seed is set.
 */
let previewTransport: ModelSettingsTransport | null = null;

/** Seed (or clear) the preview transport — dev preview only. */
export function setPreviewModelSettingsTransport(transport: ModelSettingsTransport | null): void {
  previewTransport = transport;
}

/** The native model-settings transport, or `null` in a browser-only session. */
export function createModelSettingsTransport(bindings?: ModelSettingsTransportBindings): ModelSettingsTransport | null {
  if (previewTransport) return previewTransport;
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriModelSettingsTransport(bindings ?? { invoke, listen });
}