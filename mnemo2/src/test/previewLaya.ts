import {
  setPreviewLayaTransport,
  type DispatchRoutingSettings,
  type LayaEvent,
  type LayaStatus,
  type LayaTransport,
  type RoutingPatch,
  type RoutingStatusWire,
} from "../lib/layaSettings";

/**
 * Dev-only seed for the `settings-laya` preview scene: the sample Laya
 * status the Decisions page renders, plus an in-memory fake of the IPC
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
    cheap: "openrouter/glm-4.6-flash",
    routine: "openrouter/glm-4.6-flash",
    capable: "openrouter/gpt-5.2",
    lane_medium: "openrouter/glm-5.3-flash",
    lane_high: "openrouter/gpt-5.2",
    escalate: null,
    threshold: 0.8,
    enforce: false,
  },
};

/** The in-memory transport: a clone of the sample status, a streaming setup, a real-shaped save. */
function createPreviewLayaTransport(): LayaTransport {
  let status = structuredClone(READY);
  let handlers: ((event: LayaEvent) => void)[] = [];
  const emit = (event: LayaEvent): void => {
    for (const handler of [...handlers]) handler(event);
  };
  const lane = (ref: { endpoint: string; model: string } | null): string | null =>
    ref ? `${ref.endpoint}/${ref.model}` : null;
  let routingStatus: RoutingStatusWire = {
    settings: {
      mode: "off",
      min_confidence_3: 0.475,
      min_confidence_2: 0.6,
      fallback_tier: "average",
      nudges: true,
      min_words: 20,
      effort: {},
    },
    summary: {
      decisions_by_mode: {},
      agreement_rate: null,
      laya_accuracy: null,
      lead_accuracy: null,
      labelled: 0,
      overrides_by_source: {},
      nudges: 0,
      nudges_acted_on: 0,
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
          }
          emit(event);
        }, delay);
      }
    },
    disable: async () => {
      status = { ...status, enabled: false, status: "disabled" };
    },
    save: async (flags, routing) => {
      if (flags) status = { ...status, flags: { ...status.flags, ...flags } };
      if (routing) {
        const patch: RoutingPatch = routing;
        status = {
          ...status,
          routing: {
            cheap: lane(patch.cheap),
            routine: lane(patch.routine),
            capable: lane(patch.capable),
            lane_medium: lane(patch.lane_medium),
            lane_high: lane(patch.lane_high),
            escalate: lane(patch.escalate),
            threshold: patch.threshold,
            enforce: patch.enforce,
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

/** Wire the preview transport for the `settings-laya` scene; returns the restore. */
export function seedPreviewLaya(): () => void {
  setPreviewLayaTransport(createPreviewLayaTransport());
  return () => setPreviewLayaTransport(null);
}