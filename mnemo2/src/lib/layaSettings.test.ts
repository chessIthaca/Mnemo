import { beforeEach, describe, expect, it } from "vitest";
import {
  LAYA_FLAG_META,
  LAYA_STATUS_META,
  checkSummaryText,
  createLayaTransport,
  createTauriLayaTransport,
  lowCoverage,
  setPreviewLayaTransport,
  type EvalRecord,
  type LayaStatusWord,
  type LayaTransport,
  type LayaTransportBindings,
  type TiersTopControl,
} from "./layaSettings";

/** A ready top-control wire, for the transport tests. */
const CONTROL: TiersTopControl = {
  choice: "suggest",
  mnemo1_enforce: false,
  laya_routing: true,
  laya_status: "ready",
  checkpoint: "Laya (English)",
  knn_rows: 42,
  knn_min_rows: 30,
};

/** The last check's record, as `routing_settings_status` reports it. */
const RECORD: EvalRecord = {
  date: "2026-11-05",
  min_confidence_4: 0.5,
  min_confidence_3: 0.475,
  min_confidence_2: 0.6,
  routed_share: 0.017,
  accuracy: 1,
  fingerprint: null,
};

/** The Tauri bindings with every invoke recorded, for the transport tests. */
function recordingBindings() {
  const calls: { command: string; payload: Record<string, unknown> }[] = [];
  const bindings = {
    invoke: (async (command: string, payload?: Record<string, unknown>) => {
      calls.push({ command, payload: (payload as Record<string, unknown> | undefined) ?? {} });
      return CONTROL;
    }) as unknown as LayaTransportBindings["invoke"],
    listen: (async () => () => {}) as unknown as LayaTransportBindings["listen"],
  };
  return { calls, bindings };
}

describe("layaSettings", () => {
  beforeEach(() => setPreviewLayaTransport(null));

  it("is null in a browser-only session until the preview seeds a transport", () => {
    expect(createLayaTransport()).toBeNull();
    const seeded = {} as LayaTransport;
    setPreviewLayaTransport(seeded);
    expect(createLayaTransport()).toBe(seeded);
    setPreviewLayaTransport(null);
    expect(createLayaTransport()).toBeNull();
  });

  it("lists the six switches with one plain sentence each, and never the routing flag", () => {
    expect(LAYA_FLAG_META.map((flag) => flag.key)).toEqual([
      "auto_type_memories",
      "failure_triage",
      "failure_triage_knn",
      "steer_tool_choice",
      "reflex",
      "auto_finetune",
    ]);
    // Every flag carries a plain one-line explanation.
    for (const flag of LAYA_FLAG_META) expect(flag.explanation.trim()).not.toBe("");
    // The `routing` flag rides the top control; it is not one of the switches.
    expect(LAYA_FLAG_META.some((flag) => flag.key === "routing")).toBe(false);
  });

  it("words every status", () => {
    const words: LayaStatusWord[] = ["disabled", "not_installed", "installing", "starting", "ready", "failed"];
    expect(new Set(words).size).toBe(6);
    for (const word of words) {
      expect(LAYA_STATUS_META[word].trim()).not.toBe("");
    }
    expect(LAYA_STATUS_META.disabled).toBe("Off");
    expect(LAYA_STATUS_META.not_installed).toBe("Not installed");
  });

  it("words the check summary, and promises the first check before there is one", () => {
    expect(checkSummaryText(null)).toBe("Not checked yet. Laya checks itself when a project opens.");
    expect(checkSummaryText(RECORD)).toBe(
      "Last checked 2026-11-05: confident on 2% of test tasks, 100% of those right.",
    );
  });

  it("flags low coverage only when Laya answers almost nothing", () => {
    expect(lowCoverage(null)).toBe(false);
    expect(lowCoverage(RECORD)).toBe(true);
    expect(lowCoverage({ ...RECORD, routed_share: 0.5 })).toBe(false);
    expect(lowCoverage({ ...RECORD, routed_share: null })).toBe(false);
  });

  it("wires the top control to its two commands, sending the choice by name", async () => {
    const { calls, bindings } = recordingBindings();
    const transport = createTauriLayaTransport(bindings);
    await transport.topControl();
    await transport.saveTopControl("shadow");
    expect(calls.map((call) => call.command)).toEqual([
      "tiers_top_control_status",
      "save_tiers_top_control",
    ]);
    expect(calls[1].payload).toEqual({ choice: "shadow" });
  });

  it("saves one flag through save_laya_settings and starts the check through its own command", async () => {
    const { calls, bindings } = recordingBindings();
    const transport = createTauriLayaTransport(bindings);
    await transport.saveFlag("reflex", true);
    await transport.checkLaya();
    expect(calls.map((call) => call.command)).toEqual(["save_laya_settings", "laya_check_start"]);
    // One flag at a time, and never the routing gates — the check owns those.
    expect(calls[0].payload).toEqual({ flags: { reflex: true }, routing: null });
  });
});
