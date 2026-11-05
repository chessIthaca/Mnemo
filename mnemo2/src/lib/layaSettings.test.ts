import { beforeEach, describe, expect, it } from "vitest";
import {
  DISPATCH_TIERS,
  LAYA_FLAG_META,
  LAYA_REFLEX_META,
  LAYA_STATUS_META,
  createLayaTransport,
  createTauriLayaTransport,
  layaStatusHint,
  routingDraft,
  routingWire,
  setPreviewLayaTransport,
  type LayaFlagKey,
  type LayaStatusWord,
  type LayaTransport,
  type LayaTransportBindings,
  type Mnemo1Routing,
  type TiersTopControl,
} from "./layaSettings";

/** The Laya section's five wire flag keys — `routing` rides the top control
 * and `reflex` renders with Mnemo1's routing gates. */
const FIVE_KEYS: LayaFlagKey[] = [
  "auto_type_memories",
  "failure_triage",
  "failure_triage_knn",
  "auto_finetune",
  "steer_tool_choice",
];

/** Mnemo1's routing gates as `laya_status` reports them. */
const GATES: Mnemo1Routing = {
  threshold: 0.8,
  first_prompt_min_confidence: null,
  enforce: false,
};

/** A ready top-control wire, for the transport test. */
const CONTROL: TiersTopControl = {
  choice: "suggest",
  mnemo1_enforce: false,
  laya_routing: true,
  laya_status: "ready",
  checkpoint: "Laya (English)",
  knn_rows: 42,
  knn_min_rows: 30,
};

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

  it("describes the Laya section's five flags; the routing flag rides the top control and the reflex its own meta", () => {
    expect(LAYA_FLAG_META.map((flag) => flag.key).sort()).toEqual([...FIVE_KEYS].sort());
    // Every flag carries a plain one-line explanation.
    for (const flag of LAYA_FLAG_META) expect(flag.explanation.trim()).not.toBe("");
    // Only auto_finetune rides a base checkpoint; the other classifier tasks warn.
    for (const flag of LAYA_FLAG_META) {
      if (flag.key === "auto_finetune") {
        expect(flag.note).toContain("local");
        expect(flag.note).toContain("background");
      } else if (flag.key === "failure_triage_knn") {
        expect(flag.note).toBeTruthy();
      } else {
        expect(flag.note).toContain("fine-tuned checkpoint");
      }
    }
    // The reflex (Mnemo1's plan-step tiers) renders with the routing gates,
    // so it keeps its own meta and a warning note like the rest.
    expect(LAYA_REFLEX_META.key).toBe("reflex");
    expect(LAYA_REFLEX_META.explanation.trim()).not.toBe("");
    expect(LAYA_REFLEX_META.note).toContain("fine-tuned checkpoint");
  });

  it("words every status and hints each one distinctly", () => {
    const words: LayaStatusWord[] = ["disabled", "not_installed", "installing", "starting", "ready", "failed"];
    expect(new Set(words).size).toBe(6);
    for (const word of words) {
      expect(LAYA_STATUS_META[word].trim()).not.toBe("");
      expect(layaStatusHint(word).trim()).not.toBe("");
    }
    expect(LAYA_STATUS_META.disabled).toBe("Off");
    expect(LAYA_STATUS_META.not_installed).toBe("Not installed");
  });

  it("seeds a routing draft from the wire gates, with the first-prompt gate as text", () => {
    expect(routingDraft(GATES)).toEqual({ threshold: "0.8", firstPrompt: "", enforce: false });
    expect(routingDraft({ ...GATES, first_prompt_min_confidence: 0.5 })).toEqual({
      threshold: "0.8",
      firstPrompt: "0.5",
      enforce: false,
    });
  });

  it("parses the gates into the save payload, keeping enforce through unchanged", () => {
    const parsed = routingWire({ threshold: " 0.95", firstPrompt: "0.4", enforce: true });
    expect(parsed.error).toBeUndefined();
    expect(parsed.value).toEqual({ threshold: 0.95, first_prompt_min_confidence: 0.4, enforce: true });
  });

  it("clears an empty first-prompt gate back to the kernel default", () => {
    const parsed = routingWire(routingDraft(GATES));
    expect(parsed.error).toBeUndefined();
    expect(parsed.value).toEqual({ threshold: 0.8, first_prompt_min_confidence: null, enforce: false });
  });

  it("rejects a gate that is not a number from 0 to 1, naming the gate", () => {
    for (const threshold of ["", "abc", "-0.1", "1.2", "9"]) {
      const parsed = routingWire({ ...routingDraft(GATES), threshold });
      expect(parsed.value).toBeUndefined();
      expect(parsed.error).toBe("Step-lane threshold must be a number from 0 to 1");
    }
    const parsed = routingWire({ ...routingDraft(GATES), firstPrompt: "nope" });
    expect(parsed.value).toBeUndefined();
    expect(parsed.error).toBe("First-prompt gate must be a number from 0 to 1, or empty for the default");
  });

  it("lists the four pickable tiers a dispatch can run on — escalate never", () => {
    expect(DISPATCH_TIERS).toEqual(["trivial", "routine", "medium", "high"]);
  });

  it("wires the top control to its two commands, sending the choice by name", async () => {
    const calls: { command: string; payload: Record<string, unknown> }[] = [];
    const transport = createTauriLayaTransport({
      invoke: (async (command: string, payload?: Record<string, unknown>) => {
        calls.push({ command, payload: (payload as Record<string, unknown> | undefined) ?? {} });
        return CONTROL;
      }) as unknown as LayaTransportBindings["invoke"],
      listen: (async () => () => {}) as unknown as LayaTransportBindings["listen"],
    });
    await transport.topControl();
    await transport.saveTopControl("shadow");
    expect(calls.map((call) => call.command)).toEqual([
      "tiers_top_control_status",
      "save_tiers_top_control",
    ]);
    expect(calls[1].payload).toEqual({ choice: "shadow" });
  });
});
