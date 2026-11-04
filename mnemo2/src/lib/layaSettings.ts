import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isValidModelRef } from "./modelSettings";
import { isTauriRuntime } from "./runtime";

/**
 * Decision-model (Laya) settings for the Decisions page: the transport to
 * `laya_status` / `laya_setup` / `laya_disable` / `save_laya_settings`
 * (src-tauri/src/mnemo2/laya_settings.rs), the view-model metadata (the
 * status wording, the seven opt-in flags with plain explanations, the
 * routing lanes) and the routing draft/wire parsing. Wire fields are
 * snake_case (serde), like every settings DTO in this app.
 */

/** The channel `laya_setup` streams its progress on (kernel `SetupEvent`). */
export const LAYA_EVENT_CHANNEL = "mnemo2://laya-event";

// ── Wire shapes ─────────────────────────────────────────────────────────────

/** One of the seven `[general.laya]` opt-in flags the page manages. */
export type LayaFlagKey =
  | "auto_type_memories"
  | "failure_triage"
  | "failure_triage_knn"
  | "auto_finetune"
  | "steer_tool_choice"
  | "routing"
  | "reflex";

/** The seven `[general.laya]` opt-in flags as `laya_status` reports them. */
export interface LayaFlags {
  auto_type_memories: boolean;
  failure_triage: boolean;
  failure_triage_knn: boolean;
  auto_finetune: boolean;
  steer_tool_choice: boolean;
  routing: boolean;
  reflex: boolean;
}

/**
 * The `[general.routing]` block of the `laya_status` snapshot: each model
 * target as the page edits it (`"endpoint/model"` or null), the escalate
 * confidence threshold and its enforce gate.
 */
export interface RoutingLanes {
  cheap: string | null;
  routine: string | null;
  capable: string | null;
  lane_medium: string | null;
  lane_high: string | null;
  escalate: string | null;
  threshold: number;
  enforce: boolean;
}

/** The coarse runtime state `laya_status` reports. */
export type LayaStatusWord = "disabled" | "not_installed" | "installing" | "starting" | "ready" | "failed";

/** The `laya_status` snapshot — everything the Decisions page renders. */
export interface LayaStatus {
  /** The master `[general.laya]` toggle on disk. */
  enabled: boolean;
  /** Whether the managed English checkpoint is installed in this config dir. */
  installed: boolean;
  status: LayaStatusWord;
  flags: LayaFlags;
  routing: RoutingLanes;
}

/** One model target of the `routing` save payload (the `ModelRefDto` wire). */
export interface RoutingModelRef {
  endpoint: string;
  model: string;
}

/**
 * The `routing` payload of `save_laya_settings`: every lane is sent, an
 * empty one as an explicit `null` to clear it (the kernel's double-Option
 * keeps absent keys; the page always sends all five).
 */
export interface RoutingPatch {
  cheap: RoutingModelRef | null;
  routine: RoutingModelRef | null;
  capable: RoutingModelRef | null;
  lane_medium: RoutingModelRef | null;
  lane_high: RoutingModelRef | null;
  escalate: RoutingModelRef | null;
  threshold: number;
  enforce: boolean;
}

/** One setup-pipeline event from the laya channel (`SetupEvent` on the wire). */
export interface LayaEvent {
  type: "progress" | "done" | "failed";
  step: string;
  label?: string;
  /** Best-effort 0–1 completion; absent = indeterminate. */
  fraction?: number | null;
  error?: string;
}

// ── View model ───────────────────────────────────────────────────────────────

/** The status-line wording for each status word. */
export const LAYA_STATUS_META: Record<LayaStatusWord, string> = {
  disabled: "Off",
  not_installed: "Not installed",
  installing: "Installing…",
  starting: "Starting…",
  ready: "Ready",
  failed: "Failed",
};

/** One opt-in flag as the page renders it. */
export interface LayaFlagMeta {
  key: LayaFlagKey;
  label: string;
  /** One plain sentence of what turning this on does. */
  explanation: string;
  /** A short extra note — the fine-tune warning, the background training. */
  note?: string;
}

/** The warning carried by every flag that needs a fine-tuned checkpoint. */
const NEEDS_FINETUNE = "Only meaningful against a fine-tuned checkpoint — base models are near-chance on this.";

/** The seven opt-in flags in display order, with one-line explanations. */
export const LAYA_FLAG_META: readonly LayaFlagMeta[] = [
  {
    key: "auto_type_memories",
    label: "Auto-type memories",
    explanation: "When a memory is written, a confident classifier may correct its typed prefix (SPEC/DECISION/BUG/…).",
    note: NEEDS_FINETUNE,
  },
  {
    key: "failure_triage",
    label: "Failure triage",
    explanation: "Failed runs are classified transient / permanent / needs-user / flaky-test, and a confident answer steers the retry and repair paths.",
    note: NEEDS_FINETUNE,
  },
  {
    key: "failure_triage_knn",
    label: "Learn from logged failures",
    explanation: "A local kNN overlay votes on the most similar logged failures, so new dispositions apply immediately, without a fine-tune.",
    note: "Rides the local embedder, not the Laya server.",
  },
  {
    key: "auto_finetune",
    label: "Auto fine-tune",
    explanation: "At startup, when enough new labeled failures have accumulated, retrains the checkpoint and serves it.",
    note: "Trains on your local logs in the background; never blocks startup.",
  },
  {
    key: "steer_tool_choice",
    label: "Steer tool choice",
    explanation: "A confident classifier picks whether a search is for a symbol, plain text or a memory; every fallback keeps the built-in heuristics.",
    note: NEEDS_FINETUNE,
  },
  {
    key: "routing",
    label: "Model routing",
    explanation: "At the start of a turn, a confident classification runs the turn on the cheap or capable lane below.",
    note: NEEDS_FINETUNE,
  },
  {
    key: "reflex",
    label: "Reflex (lane ladder)",
    explanation: "One call answers complexity, action and risk for the same state; plan steps ride the medium, high and escalate lanes.",
    note: NEEDS_FINETUNE,
  },
];

/** One routing lane as the page renders it. */
export interface RoutingLaneMeta {
  key: "cheap" | "routine" | "capable" | "lane_medium" | "lane_high" | "escalate";
  label: string;
  blurb: string;
}

/** The five routing targets in display order, with their plain blurbs. */
export const ROUTING_LANES: readonly RoutingLaneMeta[] = [
  { key: "cheap", label: "Cheap lane", blurb: "Serves a turn or plan step classified as trivial work." },
  { key: "routine", label: "Routine lane", blurb: "Serves a turn or plan step classified as routine work — the middle of the shared three-label question." },
  { key: "capable", label: "Capable lane", blurb: "Serves a turn classified as architectural work." },
  { key: "lane_medium", label: "Medium lane", blurb: "Serves a plan step classified as medium." },
  { key: "lane_high", label: "High lane", blurb: "Serves a plan step classified as high." },
  { key: "escalate", label: "Escalate", blurb: "The escalation target when a step wants a stronger model." },
];

/** One-line explanation under the status line, per status word. */
export function layaStatusHint(status: LayaStatusWord): string {
  switch (status) {
    case "disabled":
      return "Off — the engine runs exactly as it does without Laya: no classifier calls, no server.";
    case "not_installed":
      return "Enabled, but the managed runtime is not downloaded yet. Install runs uv, a virtualenv, laya-serve and the English checkpoint.";
    case "installing":
      return "Downloading the managed runtime — the bar follows its progress.";
    case "starting":
      return "Starting the local laya-serve server…";
    case "ready":
      return "Ready — the local server answers the engine's decision calls.";
    case "failed":
      return "The pipeline failed. Enable restarts it; the error shows what gave up.";
  }
}

// ── Draft parsing ────────────────────────────────────────────────────────────

/** The routing block as the page edits it: pickers as free text, the threshold as text. */
export interface RoutingDraft {
  cheap: string;
  routine: string;
  capable: string;
  lane_medium: string;
  lane_high: string;
  escalate: string;
  threshold: string;
  enforce: boolean;
}

/** Seed a routing draft from the wire lanes. */
export function routingDraft(lanes: RoutingLanes): RoutingDraft {
  return {
    cheap: lanes.cheap ?? "",
    routine: lanes.routine ?? "",
    capable: lanes.capable ?? "",
    lane_medium: lanes.lane_medium ?? "",
    lane_high: lanes.lane_high ?? "",
    escalate: lanes.escalate ?? "",
    threshold: String(lanes.threshold),
    enforce: lanes.enforce,
  };
}

/** Parse `"endpoint/model"` into its wire ref — the first slash splits, model ids may carry their own. */
function splitModelRef(text: string): RoutingModelRef {
  const slash = text.indexOf("/");
  return { endpoint: text.slice(0, slash), model: text.slice(slash + 1) };
}

/** Parse the confidence threshold: a number from 0 to 1, or null. */
function parseThreshold(text: string): number | null {
  const trimmed = text.trim();
  if (!/^\d+(?:\.\d+)?$/.test(trimmed)) return null;
  const parsed = Number(trimmed);
  return parsed >= 0 && parsed <= 1 ? parsed : null;
}

/**
 * Parse a routing draft into the `save_laya_settings` payload, or the
 * first field error. An empty picker clears its lane (`null`); a lane's
 * ref must be `"endpoint/model"`; the threshold must be a number from 0 to 1.
 */
export function routingWire(draft: RoutingDraft): { value: RoutingPatch; error?: undefined } | { value?: undefined; error: string } {
  const pickers: { key: keyof Omit< RoutingDraft, "threshold" | "enforce" >; label: string; text: string }[] = [
    { key: "cheap", label: "Cheap lane", text: draft.cheap },
    { key: "routine", label: "Routine lane", text: draft.routine },
    { key: "capable", label: "Capable lane", text: draft.capable },
    { key: "lane_medium", label: "Medium lane", text: draft.lane_medium },
    { key: "lane_high", label: "High lane", text: draft.lane_high },
    { key: "escalate", label: "Escalate", text: draft.escalate },
  ];
  const value = {} as Record<string, RoutingModelRef | null>;
  for (const picker of pickers) {
    const text = picker.text.trim();
    if (text === "") {
      value[picker.key] = null;
    } else if (isValidModelRef(text)) {
      value[picker.key] = splitModelRef(text);
    } else {
      return { error: `${picker.label} model must be "endpoint/model"` };
    }
  }
  const threshold = parseThreshold(draft.threshold);
  if (threshold === null) {
    return { error: "Threshold must be a number from 0 to 1" };
  }
  return {
    value: {
      cheap: value.cheap,
      routine: value.routine,
      capable: value.capable,
      lane_medium: value.lane_medium,
      lane_high: value.lane_high,
      escalate: value.escalate,
      threshold,
      enforce: draft.enforce,
    },
  };
}

// ── Transport ────────────────────────────────────────────────────────────────

/** The Tauri API surface the transport needs, injected for tests and previews. */
export interface LayaTransportBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

/** The IPC surface the Decisions page needs. */
export interface LayaTransport {
  /** The full status snapshot. */
  status(): Promise<LayaStatus>;
  /** Enable + install/start the managed runtime (progress streams via events). */
  setup(): Promise<void>;
  /** Flip the master toggle off and stop every laya-serve sidecar. */
  disable(): Promise<void>;
  /**
   * Save the opt-in flags and/or the routing lanes; each absent section is
   * kept. Never sends `enabled` — the backend rejects that key, and the
   * master toggle rides `setup`/`disable`.
   */
  save(flags?: LayaFlags, routing?: RoutingPatch): Promise<void>;
  /** Subscribe to the setup events; resolves to the unsubscribe function. */
  onLayaEvent(handler: (event: LayaEvent) => void): Promise<UnlistenFn>;
  /** The `[routing]` table, the log summary and the last eval record. */
  routingStatus(): Promise<RoutingStatusWire>;
  /** Save the `[routing]` table; returns the fresh snapshot. */
  saveRoutingSettings(settings: DispatchRoutingSettings): Promise<RoutingStatusWire>;
  /** Run the routing eval against the open lead's attached Laya. */
  runRoutingEval(): Promise<RoutingEvalWire>;
}

/** The real IPC transport over the Tauri bindings. */
export function createTauriLayaTransport(bindings: LayaTransportBindings): LayaTransport {
  return {
    status: () => bindings.invoke<LayaStatus>("laya_status"),
    setup: () => bindings.invoke<void>("laya_setup"),
    disable: () => bindings.invoke<void>("laya_disable"),
    save: (flags, routing) =>
      bindings.invoke<void>("save_laya_settings", { flags: flags ?? null, routing: routing ?? null }),
    onLayaEvent: (handler) =>
      bindings.listen<LayaEvent>(LAYA_EVENT_CHANNEL, (event) => handler(event.payload)),
    routingStatus: () => bindings.invoke<RoutingStatusWire>("routing_settings_status"),
    saveRoutingSettings: (settings) =>
      bindings.invoke<RoutingStatusWire>("save_routing_settings", { settings }),
    runRoutingEval: () => bindings.invoke<RoutingEvalWire>("run_routing_eval"),
  };
}

// ── Preview seam (mirrors lib/modelSettings.ts) ──────────────────────────────

/** The dev-only transport `src/test/previewLaya.ts` wires up in a browser. */
let previewLayaTransport: LayaTransport | null = null;

/** Install the preview transport (dev-only; no Tauri host in a browser). */
export function setPreviewLayaTransport(transport: LayaTransport | null): void {
  previewLayaTransport = transport;
}

/**
 * The Decisions page's transport: the Tauri one inside the desktop app,
 * the preview one when a test-side seed installed it, null in a plain
 * browser (the dialog shows its empty state).
 */
export function createLayaTransport(bindings?: LayaTransportBindings): LayaTransport | null {
  if (previewLayaTransport) return previewLayaTransport;
  if (!isTauriRuntime()) return null;
  return createTauriLayaTransport(bindings ?? { invoke, listen });
}
// ── Mnemo2 dispatch routing (plan 2ba2168a, design §4.5/§4.9) ────────────────

/** One dispatch-routing mode (design §4.5). */
export type RoutingModeName = "off" | "shadow" | "suggest" | "auto";

/** The `[routing]` table of `<config_dir>/mnemo2.toml` (design §4.9). */
export interface DispatchRoutingSettings {
  mode: RoutingModeName;
  min_confidence_3: number;
  min_confidence_2: number;
  fallback_tier: string;
  nudges: boolean;
  min_words: number;
  effort: Record<string, string>;
}

/** The dispatch routing log summarised (`routing_log_summary`). */
export interface RoutingLogSummary {
  decisions_by_mode: Record<string, number>;
  agreement_rate: number | null;
  laya_accuracy: number | null;
  lead_accuracy: number | null;
  labelled: number;
  overrides_by_source: Record<string, number>;
  nudges: number;
  nudges_acted_on: number;
  eval_date: string | null;
}

/** The last eval's suggestion, from `<training_dir>/route_eval.json`. */
export interface EvalRecord {
  date: string;
  min_confidence_3: number | null;
  min_confidence_2: number | null;
  routed_share: number | null;
  accuracy: number | null;
}

/** One row of the eval's threshold sweep. */
export interface EvalThresholdRow {
  threshold: number;
  routed: number;
  correct: number;
  routed_share: number;
  accuracy: number;
}

/** One case the eval's default gate gets wrong. */
export interface EvalMiss {
  text: string;
  expected: string | null;
  got: string | null;
  confidence: number | null;
}

/** One offered-label count's eval report. */
export interface EvalReport {
  labels: number;
  default_gate: number;
  table: EvalThresholdRow[];
  suggested: number | null;
  misses: EvalMiss[];
}

/** The `run_routing_eval` result: both runs and the record written. */
export interface RoutingEvalWire {
  three: EvalReport;
  two: EvalReport;
  record: EvalRecord;
}

/** The routing settings snapshot the page renders. */
export interface RoutingStatusWire {
  settings: DispatchRoutingSettings;
  summary: RoutingLogSummary;
  eval: EvalRecord | null;
}

/** The labelled-decision count `suggest`/`auto` unlock at (design §4.8). */
export const MODE_UNLOCK_DECISIONS = 30;

/** The tiers a dispatch can run on, easiest first. */
export const DISPATCH_TIERS: readonly string[] = ["simple", "average", "difficult"];

/** Shadow-mode decisions so far. */
export function shadowDecisions(summary: RoutingLogSummary): number {
  return summary.decisions_by_mode.shadow ?? 0;
}

/**
 * Whether a mode may be selected: `off`/`shadow` always; `suggest` once at
 * least [`MODE_UNLOCK_DECISIONS`] shadow decisions exist; `auto` once at
 * least that many decisions carry an outcome label. A locked mode is
 * reachable through the page's force link, behind a confirm.
 */
export function modeAvailable(mode: RoutingModeName, summary: RoutingLogSummary): boolean {
  switch (mode) {
    case "off":
    case "shadow":
      return true;
    case "suggest":
      return shadowDecisions(summary) >= MODE_UNLOCK_DECISIONS;
    case "auto":
      return summary.labelled >= MODE_UNLOCK_DECISIONS;
  }
}

/** Why a mode is locked, for the force link's confirm text. */
export function modeUnlockText(mode: RoutingModeName, summary: RoutingLogSummary): string {
  if (mode === "suggest") {
    return `${shadowDecisions(summary)} of ${MODE_UNLOCK_DECISIONS} shadow decisions`;
  }
  return `${summary.labelled} of ${MODE_UNLOCK_DECISIONS} decisions with outcome labels`;
}

/** A share as a whole-percent string, or an em dash when unset. */
export function shareText(share: number | null): string {
  return share === null ? "—" : `${Math.round(share * 100)}%`;
}

/** The gates' provenance line: the eval's date, or the provisional warning. */
export function evalDateText(record: EvalRecord | null): string {
  return record === null ? "provisional defaults, no eval yet" : `eval ${record.date}`;
}

// ── Leaving with unsaved edits ────────────────────────────────────────────────

/**
 * Whether a captured DOM event is the user leaving the Decisions page: an
 * Escape keypress (the settings dialog closes on it) or a click on a button
 * outside the page's own subtree — the settings rail or the dialog's close
 * button and backdrop. The page asks for a confirm before either one drops
 * unsaved dispatch edits; every click inside the page keeps its own meaning.
 */
export function leavesPage(event: Event, root: HTMLElement | null): boolean {
  if (event instanceof KeyboardEvent) return event.key === "Escape";
  if (event.type !== "click") return false;
  const target = event.target;
  if (!(target instanceof Element)) return false;
  if (root !== null && root.contains(target)) return false;
  return target.closest("button") !== null;
}
