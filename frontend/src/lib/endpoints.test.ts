// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";
import { endpointForModel } from "./endpoints";
import type { EndpointInfo } from "./tauri";

/** A minimal EndpointInfo — endpointForModel only reads name + models. */
function ep(name: string, models: string[]): EndpointInfo {
  return {
    name,
    kind: "OpenAI",
    base_url: `https://${name}.example.com/v1/`,
    models,
    model_configs: [],
    max_context: null,
    max_output_tokens: null,
    multimodal: false,
    supports_reasoning_effort: true,
    reasoning_effort: null,
  };
}

describe("endpointForModel (per-agent model → endpoint resolution)", () => {
  it("finds the endpoint whose models list the id", () => {
    const endpoints = [ep("openai", ["gpt-4o", "gpt-5"]), ep("ollama", ["grok"])];
    expect(endpointForModel(endpoints, "gpt-5")).toBe("openai");
    expect(endpointForModel(endpoints, "grok")).toBe("ollama");
  });

  it("returns null for an unknown model id", () => {
    expect(endpointForModel([ep("openai", ["gpt-4o"])], "ghost-model")).toBeNull();
  });

  it("returns the first match when two endpoints list the same id", () => {
    const endpoints = [ep("a", ["shared"]), ep("b", ["shared"])];
    expect(endpointForModel(endpoints, "shared")).toBe("a");
  });

  it("returns null for an empty model id and for an empty endpoint list", () => {
    expect(endpointForModel([ep("a", ["m"])], "")).toBeNull();
    expect(endpointForModel([], "m")).toBeNull();
  });
});
