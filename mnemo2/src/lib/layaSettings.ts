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
  key: "cheap" | "capable" | "lane_medium" | "lane_high" | "escalate";
  label: string;
  blurb: string;
}

/** The five routing targets in display order, with their plain blurbs. */
export const ROUTING_LANES: readonly RoutingLaneMeta[] = [
  { key: "cheap", label: "Cheap lane", blurb: "Serves a turn or plan step classified as trivial work." },
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