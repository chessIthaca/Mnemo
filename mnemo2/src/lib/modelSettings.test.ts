import { afterEach, describe, expect, it, vi } from "vitest";
import {
  EMBEDDER_EVENT_CHANNEL,
  OPTIMIZER_LEVERS,
  budgetDraft,
  budgetWire,
  createModelSettingsTransport,
  createTauriModelSettingsTransport,
  isValidModelRef,
  modelRefOptions,
  optimizerDraft,
  optimizerWire,
  parseWholeNumber,
  setPreviewModelSettingsTransport,
  type EmbedderEvent,
  type ModelSettings,
  type ModelSettingsTransport,
} from "./modelSettings";
import { emptyModel, type EndpointDetailDto } from "./settingsEngine";

/**
 * modelSettings — the IPC wiring (command names and the embedder channel)
 * plus the draft parsing the pages save through.
 */

function sampleEndpoints(): EndpointDetailDto[] {
  return [
    {
      name: "alpha",
      kind: "openai",
      base_url: "https://alpha.example/v1/",
      has_api_key: true,
      max_context: null,
      max_output_tokens: null,
      multimodal: false,
      supports_reasoning_effort: true,
      reasoning_effort: null,
      supports_strict_schema: null,
      workspace_id: null,
      temperature: null,
      models: [emptyModel("m1"), emptyModel("m1"), emptyModel("m2")],
    },
    {
      name: "beta",
      kind: "openai",
      base_url: "https://beta.example/v1/",
      has_api_key: false,
      max_context: null,
      max_output_tokens: null,
      multimodal: false,
      supports_reasoning_effort: false,
      reasoning_effort: null,
      supports_strict_schema: null,
      workspace_id: null,
      temperature: null,
      models: [emptyModel("m1")],
    },
  ];
}

describe("modelSettings", () => {
  afterEach(() => {
    setPreviewModelSettingsTransport(null);
  });

  it("is null in a browser-only session until the preview seeds a transport", () => {
    expect(createModelSettingsTransport()).toBeNull();
    const seeded = {} as ModelSettingsTransport;
    setPreviewModelSettingsTransport(seeded);
    expect(createModelSettingsTransport()).toBe(seeded);
    setPreviewModelSettingsTransport(null);
    expect(createModelSettingsTransport()).toBeNull();
  });

  it("wires the commands and the embedder channel", async () => {
    const invoke = vi.fn();
    let channelHandler: ((event: { payload: EmbedderEvent }) => void) | null = null;
    const listen = vi.fn(async (_channel: string, handler: (event: { payload: EmbedderEvent }) => void) => {
      channelHandler = handler;
      return () => {};
    });
    const transport = createTauriModelSettingsTransport({ invoke, listen } as never);

    await transport.get();
    expect(invoke).toHaveBeenLastCalledWith("model_settings_get");

    await transport.save("vision", null);
    expect(invoke).toHaveBeenLastCalledWith("model_settings_save", { section: "vision", value: null });

    await transport.install("all-MiniLM-L6-v2");
    expect(invoke).toHaveBeenLastCalledWith("embedding_install", { modelId: "all-MiniLM-L6-v2" });

    const seen: EmbedderEvent[] = [];
    await transport.onEmbedderEvent((event) => seen.push(event));
    expect(listen.mock.calls[0][0]).toBe(EMBEDDER_EVENT_CHANNEL);
    channelHandler!({ payload: { type: "progress", step: "embedder_model", label: "Fetching", fraction: 0.5 } });
    expect(seen).toEqual([{ type: "progress", step: "embedder_model", label: "Fetching", fraction: 0.5 }]);
  });

  it("validates model references", () => {
    expect(isValidModelRef("alpha/m1")).toBe(true);
    expect(isValidModelRef(" alpha/m1 ")).toBe(true);
    expect(isValidModelRef("openrouter/org/model-name")).toBe(true);
    expect(isValidModelRef("m1")).toBe(false);
    expect(isValidModelRef("/m1")).toBe(false);
    expect(isValidModelRef("alpha/")).toBe(false);
    expect(isValidModelRef("")).toBe(false);
  });

  it("parses whole numbers", () => {
    expect(parseWholeNumber("3000")).toBe(3000);
    expect(parseWholeNumber(" 42 ")).toBe(42);
    expect(parseWholeNumber("3.5")).toBeNull();
    expect(parseWholeNumber("-1")).toBeNull();
    expect(parseWholeNumber("lots")).toBeNull();
    expect(parseWholeNumber("")).toBeNull();
  });

  it("builds the optimizer payload from a draft and names invalid knobs", () => {
    const settings: ModelSettings["optimizer"] = {
      delta_reads: true,
      compress_output: false,
      archive: true,
      compaction_survival: true,
      quality_score: true,
      lean_output_nudge: true,
      recall_delta: false,
      idle_compaction: true,
      archive_min_chars: 2400,
      compress_min_chars: 3000,
      lean_output_fill_pct: 80,
      nudge_cooldown_requests: 4,
      compress_extra_commands: ["cargo test"],
    };
    const draft = optimizerDraft(settings);
    // The idle-compaction lever rides the lever list and its switch rides the payload.
    expect(OPTIMIZER_LEVERS.find((lever) => lever.key === "idle_compaction")).toMatchObject({
      key: "idle_compaction",
      label: "Idle compaction",
    });
    expect(optimizerWire(optimizerDraft({ ...settings, idle_compaction: false })).value?.idle_compaction).toBe(false);
    draft.lean_output_fill_pct = "75";
    draft.compress_extra_commands = "cargo clippy,  npm test ";
    const parsed = optimizerWire(draft);
    expect(parsed.error).toBeUndefined();
    expect(parsed.value).toEqual({
      ...settings,
      lean_output_fill_pct: 75,
      compress_extra_commands: ["cargo clippy", "npm test"],
    });

    draft.lean_output_fill_pct = "150";
    expect(optimizerWire(draft).error).toBe("Nudge at fill % must be a whole number from 0 to 100");
    draft.lean_output_fill_pct = "80";
    draft.archive_min_chars = "big";
    expect(optimizerWire(draft).error).toBe("Archive after must be a whole number");
  });

  it("builds the budget payload from a draft and names an invalid cap", () => {
    const draft = budgetDraft({ enabled: true, max_tokens_per_plan: 2_000_000, max_escalations_per_plan: 3, max_retries_per_lane: 2 });
    draft.max_tokens_per_plan = "1500000";
    const parsed = budgetWire(draft);
    expect(parsed.value).toEqual({ enabled: true, max_tokens_per_plan: 1_500_000, max_escalations_per_plan: 3, max_retries_per_lane: 2 });
    draft.max_retries_per_lane = "often";
    expect(budgetWire(draft).error).toBe("Retries per lane must be a whole number");
  });

  it("collects picker options from the configured endpoints, deduplicated in order", () => {
    expect(modelRefOptions(sampleEndpoints())).toEqual(["alpha/m1", "alpha/m2", "beta/m1"]);
  });
});