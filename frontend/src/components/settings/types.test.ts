// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";
import {
  capsAutofillPatch,
  dirtySectionIds,
  discoveredCapsById,
  effectiveCaps,
  effortFromSelectValue,
  effortToSelectValue,
  importEndpointEditable,
  modelConfigFor,
  parseEffortsList,
  parsePositiveIntInput,
  REASONING_EFFORT_DEFAULT,
  SETTINGS_NAV,
  type SettingsSectionId,
  upsertModelConfig,
} from "./types";
import type { EndpointEditable, VisionModelInfo } from "../../lib/tauri";

/** A minimal endpoint for the per-model-config helper tests. */
function ep(models: string[]): EndpointEditable {
  return {
    name: "gateway",
    kind: "openai",
    base_url: "https://gateway.example.com/v1/",
    models,
    model_configs: [],
    max_context: null,
    max_output_tokens: null,
    multimodal: false,
    supports_reasoning_effort: true,
    reasoning_effort: null,
  };
}

describe("SETTINGS_NAV", () => {
  it("has a Sounds section (notification-sound toggles, after Chat)", () => {
    const idx = SETTINGS_NAV.findIndex((n) => n.id === "sounds");
    expect(idx).toBeGreaterThan(-1);
    expect(SETTINGS_NAV[idx].label).toBe("Sounds");
    // Placed right after Chat so the two chat-surface sections sit together.
    expect(SETTINGS_NAV[idx - 1]?.id).toBe("chat");
    // The search filter matches on keywords — sounds must be findable.
    expect(SETTINGS_NAV[idx].keywords).toContain("ding");
  });

  it("every nav id is unique", () => {
    const ids = SETTINGS_NAV.map((n) => n.id);
    expect(new Set(ids).size).toBe(ids.length);
  });
});

describe("dirtySectionIds", () => {
  it("returns an empty array for an empty map", () => {
    expect(dirtySectionIds({})).toEqual([]);
  });

  it("returns the single dirty id", () => {
    expect(dirtySectionIds({ safety: true })).toEqual(["safety"]);
  });

  it("returns all dirty ids, preserving insertion order", () => {
    const map: Partial<Record<SettingsSectionId, boolean>> = {
      providers: true,
      appearance: false,
      safety: true,
      vision: undefined,
      savings: true,
    };
    expect(dirtySectionIds(map)).toEqual(["providers", "safety", "savings"]);
  });

  it("ignores false and undefined values", () => {
    const map: Partial<Record<SettingsSectionId, boolean>> = {
      models: false,
      advanced: undefined,
      safety: true,
    };
    expect(dirtySectionIds(map)).toEqual(["safety"]);
  });

  it("returns an empty array when all values are false", () => {
    const map: Partial<Record<SettingsSectionId, boolean>> = {
      providers: false,
      appearance: false,
    };
    expect(dirtySectionIds(map)).toEqual([]);
  });
});

describe("per-model config helpers", () => {
  it("modelConfigFor returns a bare config for a model without one", () => {
    const e = ep(["gpt-4o"]);
    expect(modelConfigFor(e, "gpt-4o")).toEqual({
      id: "gpt-4o",
      max_context: null,
      max_output_tokens: null,
      reasoning_efforts: [],
      reasoning_effort: null,
    });
  });

  it("modelConfigFor finds the config by id (order drift tolerated)", () => {
    const e = ep(["gpt-4o", "mini"]);
    e.model_configs = [
      { id: "mini", max_context: 64000, max_output_tokens: 4096, reasoning_efforts: ["high", "low"] },
      { id: "gpt-4o", max_context: 200000, max_output_tokens: null, reasoning_efforts: [] },
    ];
    // Found by id even though "mini"'s config comes first in the array.
    expect(modelConfigFor(e, "gpt-4o").max_context).toBe(200000);
    expect(modelConfigFor(e, "mini").reasoning_efforts).toEqual(["high", "low"]);
  });

  it("upsertModelConfig replaces the config for the model id", () => {
    const e = ep(["gpt-4o", "mini"]);
    e.model_configs = [{ id: "gpt-4o", max_context: 128000, max_output_tokens: null, reasoning_efforts: [] }];
    const next = upsertModelConfig(e, "gpt-4o", { max_context: 200000, reasoning_efforts: ["max", "high"] });
    expect(next).toEqual([
      { id: "gpt-4o", max_context: 200000, max_output_tokens: null, reasoning_efforts: ["max", "high"] },
      { id: "mini", max_context: null, max_output_tokens: null, reasoning_efforts: [], reasoning_effort: null },
    ]);
  });

  it("upsertModelConfig prunes configs for deleted models", () => {
    const e = ep(["gpt-4o"]);
    e.model_configs = [{ id: "gpt-4o", max_context: 128000, max_output_tokens: null, reasoning_efforts: [] }];
    // "ghost" isn't in models → the upsert drops it (no orphaned config).
    const next = upsertModelConfig(e, "ghost", { max_context: 1 });
    expect(next).toEqual([
      { id: "gpt-4o", max_context: 128000, max_output_tokens: null, reasoning_efforts: [] },
    ]);
  });

  it("upsertModelConfig carries a per-model multimodal patch", () => {
    // The Vision checkbox patches per-model multimodal (backlog a634835c —
    // mixed endpoints like Ollama hosting a text-only GLM and a
    // vision-capable GLM flash); other models keep the blank/inherit shape.
    const e = ep(["glm", "glm-flash"]);
    const next = upsertModelConfig(e, "glm-flash", { multimodal: true });
    expect(next).toEqual([
      { id: "glm", max_context: null, max_output_tokens: null, reasoning_efforts: [], reasoning_effort: null },
      {
        id: "glm-flash",
        max_context: null,
        max_output_tokens: null,
        reasoning_efforts: [],
        reasoning_effort: null,
        multimodal: true,
      },
    ]);
  });

  it("upsertModelConfig carries a per-model reasoning_effort patch", () => {
    // Backlog 5b099aef — the per-model row effort dropdown: null = inherit
    // the endpoint's value; other models keep the blank/inherit shape.
    const e = ep(["glm", "glm-flash"]);
    const next = upsertModelConfig(e, "glm", { reasoning_effort: "low" });
    expect(next).toEqual([
      {
        id: "glm",
        max_context: null,
        max_output_tokens: null,
        reasoning_efforts: [],
        reasoning_effort: "low",
      },
      {
        id: "glm-flash",
        max_context: null,
        max_output_tokens: null,
        reasoning_efforts: [],
        reasoning_effort: null,
      },
    ]);
  });
});

describe("capsAutofillPatch", () => {
  it("fills null per-model fields with the discovered caps", () => {
    const e = ep(["m"]);
    const patch = capsAutofillPatch(e, "m", { ctx: 100000, out: 8000 });
    expect(patch).toEqual({
      model_configs: [
        { id: "m", max_context: 100000, max_output_tokens: 8000, reasoning_efforts: [], reasoning_effort: null },
      ],
    });
  });

  it("never overwrites an explicit per-model value", () => {
    const e = ep(["m"]);
    e.model_configs = [
      { id: "m", max_context: 128000, max_output_tokens: null, reasoning_efforts: [], reasoning_effort: "low" },
    ];
    const patch = capsAutofillPatch(e, "m", { ctx: 100000, out: 8000 });
    expect(patch).toEqual({
      model_configs: [
        { id: "m", max_context: 128000, max_output_tokens: 8000, reasoning_efforts: [], reasoning_effort: "low" },
      ],
    });
  });

  it("does not fill when the endpoint-level value is set (explicit config wins)", () => {
    const e = ep(["m"]);
    e.max_context = 128000; // explicit endpoint-level ctx — must block autofill
    const patch = capsAutofillPatch(e, "m", { ctx: 100000, out: 8000 });
    // ctx blocked by the explicit endpoint-level value; out (both null) fills per-model.
    expect(patch).toEqual({
      model_configs: [
        { id: "m", max_context: null, max_output_tokens: 8000, reasoning_efforts: [], reasoning_effort: null },
      ],
    });
  });

  it("returns null when nothing needs filling or the model is unknown", () => {
    const e = ep(["m"]);
    expect(capsAutofillPatch(e, "m", { ctx: null, out: null })).toBeNull();
    e.model_configs = [
      { id: "m", max_context: 1, max_output_tokens: 1, reasoning_efforts: [] },
    ];
    expect(capsAutofillPatch(e, "m", { ctx: 100000, out: 8000 })).toBeNull();
    expect(capsAutofillPatch(e, "ghost", { ctx: 100000, out: 8000 })).toBeNull();
  });
});

describe("importEndpointEditable", () => {
  it("maps model_configs through (per-model caps/efforts survive import)", () => {
    // Regression (review 2026-08-22 M1): the export bundle carries
    // model_configs; the import mapping must not silently drop them.
    const raw = {
      name: "ex",
      kind: "openai",
      base_url: "https://ex.com/v1/",
      models: ["m"],
      model_configs: [
        {
          id: "m",
          max_context: 100000,
          max_output_tokens: 8000,
          reasoning_efforts: ["high"],
          multimodal: true,
        },
      ],
      max_context: null,
      max_output_tokens: null,
      multimodal: false,
    };
    const e = importEndpointEditable(raw);
    // The per-model vision flag rides along (backlog a634835c); absent →
    // null ("inherit the endpoint flag").
    expect(e.model_configs).toEqual([
      {
        id: "m",
        max_context: 100000,
        max_output_tokens: 8000,
        reasoning_efforts: ["high"],
        reasoning_effort: null,
        multimodal: true,
      },
    ]);
  });

  it("maps per-model reasoning_effort through (unset → null = inherit)", () => {
    // Backlog 5b099aef: the per-model effort default must survive re-import
    // (same class as review 2026-08-22 M1 — dropped fields never come back).
    const raw = {
      name: "ex",
      kind: "openai",
      base_url: "https://ex.com/v1/",
      models: ["glm", "glm-flash"],
      model_configs: [{ id: "glm", reasoning_effort: "low" }, { id: "glm-flash" }],
    };
    const e = importEndpointEditable(raw);
    expect(e.model_configs?.[0]?.reasoning_effort).toBe("low");
    expect(e.model_configs?.[1]?.reasoning_effort).toBeNull();
  });

  it("defaults model_configs to empty on legacy exports", () => {
    const e = importEndpointEditable({ name: "ex", models: ["m"] });
    expect(e.model_configs).toEqual([]);
    expect(e.supports_reasoning_effort).toBe(true);
    expect(e.kind).toBe("openai");
  });
});

describe("effort select mapping", () => {
  it("effortToSelectValue maps null/undefined/empty to the default sentinel", () => {
    expect(effortToSelectValue(null)).toBe(REASONING_EFFORT_DEFAULT);
    expect(effortToSelectValue(undefined)).toBe(REASONING_EFFORT_DEFAULT);
    expect(effortToSelectValue("")).toBe(REASONING_EFFORT_DEFAULT);
  });

  it("effortToSelectValue passes stored efforts through verbatim (off stays off)", () => {
    expect(effortToSelectValue("off")).toBe("off");
    expect(effortToSelectValue("low")).toBe("low");
    expect(effortToSelectValue("high")).toBe("high");
  });

  it("effortFromSelectValue maps the sentinel back to null (never persists the sentinel)", () => {
    // Returning "" instead of null here would persist "__default__" into
    // endpoints.toml — the sentinel is a select-only value.
    expect(effortFromSelectValue(REASONING_EFFORT_DEFAULT)).toBeNull();
  });

  it("effortFromSelectValue passes concrete efforts through (off stays off)", () => {
    expect(effortFromSelectValue("off")).toBe("off");
    expect(effortFromSelectValue("high")).toBe("high");
  });

  it("round-trips the stored domain: from(to(x)) === x", () => {
    // The stored domain is null + the concrete effort strings. A regression
    // here silently rewrites stored configs (persisting the sentinel string
    // instead of null, or dropping the "off" passthrough breaks off-mode).
    const domain = [null, "off", "low", "medium", "high"] as const;
    for (const x of domain) {
      expect(effortFromSelectValue(effortToSelectValue(x))).toBe(x);
    }
  });
});

describe("endpoint caps + input parsing", () => {
  it("discoveredCapsById maps the fetched list, null-coalescing absent fields", () => {
    const list: VisionModelInfo[] = [
      { id: "gpt-4o", vision_capable: true, context_length: 128000, max_output_tokens: 16384 },
      { id: "mystery", vision_capable: false },
    ];
    expect(discoveredCapsById(list)).toEqual({
      "gpt-4o": { ctx: 128000, out: 16384 },
      mystery: { ctx: null, out: null },
    });
    expect(discoveredCapsById([])).toEqual({});
  });

  it("effectiveCaps: per-model override wins, else the endpoint-level value", () => {
    const e = ep(["m", "m2"]);
    e.max_context = 64000;
    e.max_output_tokens = 4096;
    e.model_configs = [
      { id: "m", max_context: 200000, max_output_tokens: null, reasoning_efforts: [] },
    ];
    expect(effectiveCaps(e, "m")).toEqual({ ctx: 200000, out: 4096 });
    expect(effectiveCaps(e, "m2")).toEqual({ ctx: 64000, out: 4096 });
  });

  it("parseEffortsList splits on commas, trims, and drops empties", () => {
    expect(parseEffortsList("max, high")).toEqual(["max", "high"]);
    expect(parseEffortsList(" max ,high,, ")).toEqual(["max", "high"]);
    expect(parseEffortsList("")).toEqual([]);
    expect(parseEffortsList(",,")).toEqual([]);
  });

  it("parsePositiveIntInput: clear → null, valid → floor, invalid → undefined (no change)", () => {
    expect(parsePositiveIntInput("")).toBeNull();
    expect(parsePositiveIntInput("42")).toBe(42);
    expect(parsePositiveIntInput("3.9")).toBe(3);
    expect(parsePositiveIntInput("0")).toBeUndefined();
    expect(parsePositiveIntInput("-5")).toBeUndefined();
    expect(parsePositiveIntInput("abc")).toBeUndefined();
  });
});
