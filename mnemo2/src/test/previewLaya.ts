import {
  setPreviewLayaTransport,
  type DispatchRoutingSettings,
  type LayaEvent,
  type LayaStatus,
  type LayaTransport,
  type RoutingStatusWire,
  type TierChoice,
  type TiersTopControl,
} from "../lib/layaSettings";

/**
 * Dev-only seed for the `settings-tiers` preview scene: the sample Laya
 * status the Tiers page renders, plus an in-memory fake of the IPC
 * transport (no Tauri host in a browser). This file lives under src/test/
 * so the no-mock guard keeps it out of the shipped app; the scene mounts
 * the real SettingsDialog against this fake.
 */

/** The status the scene opens on: enabled, installed, ready. */
const READY: LayaStatus = {
  enabled: true,
  installed: true,
  status: "ready",
  flags: {
    auto_type_memories: true,
    failure_triage: true,
    failure_triage_knn: false,
    auto_finetune: true,
    steer_tool_choice: false,
    routing: true,
    reflex: false,
  },
  routing: {
    threshold: 0.8,
    first_prompt_min_confidence: null,
    enforce: false,
  },
};

/** The top control the scene opens on: Suggest, Laya ready, kNN warm. */
const CONTROL: TiersTopControl = {
  choice: "suggest",
  mnemo1_enforce: false,
  laya_routing: true,
  laya_status: "ready",
  checkpoint: "Laya (English)",
  knn_rows: 42,
  knn_min_rows: 30,
};

/** The in-memory transport: a clone of the sample status, a streaming setup, a real-shaped save. */
function createPreviewLayaTransport(): LayaTransport {
  let status = structuredClone(READY);
  let control = structuredClone(CONTROL);
  let handlers: ((event: LayaEvent) => void)[] = [];
  const emit = (event: LayaEvent): void => {
    for (const handler of [...handlers]) handler(event);
  };
  let routingStatus: RoutingStatusWire = {
    settings: {
      mode: "suggest",
      min_confidence_4: 0.4,
      min_confidence_3: 0.475,
      min_confidence_2: 0.6,
      knn_min_share: 0.8,
      knn_min_rows: 30,
      fallback_tier: "high",
      nudges: true,
      min_words: 20,
      effort: {},
    },
    summary: {
      decisions_by_mode: { shadow: 31, suggest: 8 },
      agreement_rate: 0.84,
      laya_accuracy: 0.9,
      lead_accuracy: 0.75,
      labelled: 42,
      overrides_by_source: { review: 1 },
      nudges: 3,
      nudges_acted_on: 1,
      eval_date: null,
    },
    eval: null,
  };
  return {
    status: async () => structuredClone(status),
    routingStatus: async () => structuredClone(routingStatus),
    saveRoutingSettings: async (settings: DispatchRoutingSettings) => {
      routingStatus = { ...routingStatus, settings: structuredClone(settings) };
      return structuredClone(routingStatus);
    },
    runRoutingEval: async () => {
      throw new Error("the routing eval needs the desktop app: it asks the lead's attached Laya");
    },
    topControl: async () => structuredClone(control),
    saveTopControl: async (choice: TierChoice) => {
      const enforce = choice === "auto";
      const layaRouting = choice !== "off";
      control = {
        ...control,
        choice,
        mnemo1_enforce: enforce,
        laya_routing: layaRouting,
      };
      routingStatus = {
        ...routingStatus,
        settings: { ...routingStatus.settings, mode: choice },
      };
      status = {
        ...status,
        routing: { ...status.routing, enforce },
        flags: { ...status.flags, routing: layaRouting },
      };
      return structuredClone(control);
    },
    setup: async () => {
      status.status = "installing";
      const events: [number, LayaEvent][] = [
        [200, { type: "progress", step: "laya_runtime", label: "Downloading uv + the virtualenv", fraction: 0.15 }],
        [700, { type: "progress", step: "laya_runtime", label: "Installing laya[serve]", fraction: 0.5 }],
        [1300, { type: "progress", step: "laya_runtime", label: "Downloading the English checkpoint", fraction: 0.85 }],
        [1900, { type: "done", step: "laya_runtime" }],
      ];
      for (const [delay, event] of events) {
        setTimeout(() => {
          if (event.type === "done") {
            status = { ...status, enabled: true, installed: true, status: "ready" };
            control = { ...control, laya_status: "ready" };
          }
          emit(event);
        }, delay);
      }
    },
    disable: async () => {
      status = { ...status, enabled: false, status: "disabled" };
      control = { ...control, laya_status: "disabled" };
    },
    save: async (flags, routing) => {
      if (flags) status = { ...status, flags: { ...status.flags, ...flags } };
      if (routing) {
        status = {
          ...status,
          routing: {
            threshold: routing.threshold,
            first_prompt_min_confidence: routing.first_prompt_min_confidence,
            enforce: routing.enforce,
          },
        };
      }
    },
    onLayaEvent: async (handler) => {
      handlers.push(handler);
      return () => {
        handlers = handlers.filter((entry) => entry !== handler);
      };
    },
  };
}

/** Wire the preview transport for the `settings-tiers` scene; returns the restore. */
export function seedPreviewLaya(): () => void {
  setPreviewLayaTransport(createPreviewLayaTransport());
  return () => setPreviewLayaTransport(null);
}
