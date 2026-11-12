import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";

/**
 * Decision-model (Laya) settings for the Tiers page: the transport to
 * `laya_status` / `laya_setup` / `laya_disable` / `save_laya_settings` /
 * `laya_check_start` (src-tauri/src/mnemo2/laya_settings.rs), the top
 * control's wire, the view-model metadata (the status wording, the opt-in
 * flags with plain explanations) and the Laya check's summary wording.
 * Wire fields are snake_case (serde), like every settings DTO in this app.
 */

/** The channel `laya_setup` streams its progress on (kernel `SetupEvent`). */
export const LAYA_EVENT_CHANNEL = "mnemo2://laya-event";

// ── Wire shapes ─────────────────────────────────────────────────────────────

/** One of the four `[general.laya]` opt-in flags the page manages. */
export type LayaFlagKey =
  | "auto_type_memories"
  | "failure_triage"
  | "failure_triage_knn"
  | "steer_tool_choice";

/** The four `[general.laya]` opt-in flags as `laya_status` reports them. */
export interface LayaFlags {
  auto_type_memories: boolean;
  failure_triage: boolean;
  failure_triage_knn: boolean;
  steer_tool_choice: boolean;
}

/** The coarse runtime state `laya_status` reports. */
export type LayaStatusWord = "disabled" | "not_installed" | "installing" | "starting" | "ready" | "failed";

/** The `laya_status` snapshot — everything the Tiers page renders. */
export interface LayaStatus {
  /** The master `[general.laya]` toggle on disk. */
  enabled: boolean;
  /** Whether the managed English checkpoint is installed in this config dir. */
  installed: boolean;
  status: LayaStatusWord;
  flags: LayaFlags;
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
}

/** The collapsed "Other things Laya can do" section's flags in display order,
 * with one-line explanations — the kernel's per-turn routing switches never
 * ride this list. */
export const LAYA_FLAG_META: readonly LayaFlagMeta[] = [
  {
    key: "auto_type_memories",
    label: "Auto-type memories",
    explanation: "When a memory is written, a confident classifier may correct its typed prefix (SPEC/DECISION/BUG/…).",
  },
  {
    key: "failure_triage",
    label: "Failure triage",
    explanation: "Failed runs are classified transient / permanent / needs-user / flaky-test, and a confident answer steers the retry and repair paths.",
  },
  {
    key: "failure_triage_knn",
    label: "Learn from logged failures",
    explanation: "A local kNN overlay votes on the most similar logged failures, so new dispositions apply immediately, without a fine-tune.",
  },
  {
    key: "steer_tool_choice",
    label: "Steer tool choice",
    explanation: "A confident classifier picks whether a search is for a symbol, plain text or a memory; every fallback keeps the built-in heuristics.",
  },
];

// ── Transport ────────────────────────────────────────────────────────────────

/** The Tauri API surface the transport needs, injected for tests and previews. */
export interface LayaTransportBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

/** The IPC surface the Tiers page needs. */
export interface LayaTransport {
  /** The full status snapshot. */
  status(): Promise<LayaStatus>;
  /** Enable + install/start the managed runtime (progress streams via events). */
  setup(): Promise<void>;
  /** Flip the master toggle off and stop every laya-serve sidecar. */
  disable(): Promise<void>;
  /** Save one opt-in flag; the page sends nothing else in the same call. */
  saveFlag(key: LayaFlagKey, value: boolean): Promise<void>;
  /** Subscribe to the setup events; resolves to the unsubscribe function. */
  onLayaEvent(handler: (event: LayaEvent) => void): Promise<UnlistenFn>;
  /** The `[routing]` table, the log summary and the last check's record. */
  routingStatus(): Promise<RoutingStatusWire>;
  /** Run the Laya check now, even when the last one is still up to date. */
  checkLaya(): Promise<void>;
  /** The top control's status: the saved choice plus Laya's readiness (design §7.1). */
  topControl(): Promise<TiersTopControl>;
  /** Save the top control's choice; returns the fresh wire. */
  saveTopControl(choice: TierChoice): Promise<TiersTopControl>;
}

/** The real IPC transport over the Tauri bindings. */
export function createTauriLayaTransport(bindings: LayaTransportBindings): LayaTransport {
  return {
    status: () => bindings.invoke<LayaStatus>("laya_status"),
    setup: () => bindings.invoke<void>("laya_setup"),
    disable: () => bindings.invoke<void>("laya_disable"),
    saveFlag: (key, value) =>
      bindings.invoke<void>("save_laya_settings", { flags: { [key]: value } }),
    onLayaEvent: (handler) =>
      bindings.listen<LayaEvent>(LAYA_EVENT_CHANNEL, (event) => handler(event.payload)),
    routingStatus: () => bindings.invoke<RoutingStatusWire>("routing_settings_status"),
    checkLaya: () => bindings.invoke<void>("laya_check_start"),
    topControl: () => bindings.invoke<TiersTopControl>("tiers_top_control_status"),
    saveTopControl: (choice) =>
      bindings.invoke<TiersTopControl>("save_tiers_top_control", { choice }),
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
 * The Tiers page's transport: the Tauri one inside the desktop app,
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

/** One answer of the Settings > Tiers top control (design §7.1) — the
 * mnemo2.toml `[routing]` mode the save writes. */
export type TierChoice = RoutingModeName;

/** The top control's status (`tiers_top_control_status`): the saved choice
 * plus Laya's readiness for the choices that need it. */
export interface TiersTopControl {
  /** The saved choice (`off` = the lead picks). */
  choice: TierChoice;
  /** Laya's coarse readiness word. */
  laya_status: LayaStatusWord;
  /** The installed checkpoint's name, when one is installed. */
  checkpoint: string | null;
  /** Labelled workers the kNN has indexed (0 with no team open). */
  knn_rows: number;
  /** The labelled-worker count the kNN answers from (`knn_min_rows`). */
  knn_min_rows: number;
}

/** The `[routing]` table of `<config_dir>/mnemo2.toml` (design §4.9). */
export interface DispatchRoutingSettings {
  mode: RoutingModeName;
  min_confidence_4: number;
  min_confidence_3: number;
  min_confidence_2: number;
  knn_min_share: number;
  knn_min_rows: number;
  fallback_tier: string;
  nudges: boolean;
  min_words: number;
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

/** The last Laya check's record, from `<training_dir>/route_eval.json`. */
export interface EvalRecord {
  date: string;
  min_confidence_4: number | null;
  min_confidence_3: number | null;
  min_confidence_2: number | null;
  routed_share: number | null;
  accuracy: number | null;
  /** The inputs the check ran against (test set + checkpoint); a different
   * one means the record is stale and the check runs again. */
  fingerprint?: string | null;
}

/** The routing settings snapshot the page renders. */
export interface RoutingStatusWire {
  settings: DispatchRoutingSettings;
  summary: RoutingLogSummary;
  eval: EvalRecord | null;
}

/** The labelled-decision count `suggest`/`auto` unlock at (design §4.8). */
export const MODE_UNLOCK_DECISIONS = 30;

/** Shadow-mode decisions so far. */
export function shadowDecisions(summary: RoutingLogSummary): number {
  return summary.decisions_by_mode.shadow ?? 0;
}

/**
 * Whether a mode is past its unlock count: `off`/`shadow` always;
 * `suggest` once at least [`MODE_UNLOCK_DECISIONS`] shadow decisions
 * exist; `auto` once at least that many decisions carry an outcome
 * label. A below-count mode is still pickable — its card shows a hint
 * about what it is waiting for.
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

/** Why a mode is below its unlock count, for its card's hint. */
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

/** The one-line summary of the last check — or the promise of the first. */
export function checkSummaryText(record: EvalRecord | null): string {
  if (record === null) return "Not checked yet. Laya checks itself when a project opens.";
  return `Last checked ${record.date}: confident on ${shareText(record.routed_share)} of test tasks, ${shareText(record.accuracy)} of those right.`;
}

/** True when the check says Laya will leave most choices to the lead. */
export function lowCoverage(record: EvalRecord | null): boolean {
  return record !== null && record.routed_share !== null && record.routed_share < 0.2;
}
