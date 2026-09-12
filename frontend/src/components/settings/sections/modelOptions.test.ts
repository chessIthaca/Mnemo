// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Pricing model combobox — data-layer unit tests (backlog 82dd66fc). The
 * dropdown must list the models available from the configured endpoints,
 * grouped/deduped across them (a model id can repeat — entries are
 * annotated with the endpoint names) and sorted; the filter rule keeps an
 * exact match browsing-friendly. Node environment — pure functions, no DOM.
 */

import { describe, expect, it } from "vitest";
import {
  buildModelOptions,
  filterModelOptions,
  type ModelOption,
} from "./modelOptions";
import type { EndpointInfo } from "../../../lib/tauri";

function ep(name: string, models: string[]): EndpointInfo {
  return { name, kind: "openai", base_url: "https://example.test/v1", models };
}

describe("buildModelOptions", () => {
  it("lists every configured model, sorted by id", () => {
    const options = buildModelOptions([
      ep("zeta", ["glm-5.3-gcp", "glm-5.2"]),
      ep("alpha", ["deepseek-v4-flash"]),
    ]);
    expect(options.map((o) => o.model)).toEqual([
      "deepseek-v4-flash",
      "glm-5.2",
      "glm-5.3-gcp",
    ]);
  });

  it("groups a model served by several endpoints into one annotated entry", () => {
    const options = buildModelOptions([
      ep("openrouter", ["glm-5.3-gcp", "glm-5.2"]),
      ep("lmstudio", ["glm-5.3-gcp"]),
    ]);
    const shared = options.find((o) => o.model === "glm-5.3-gcp");
    expect(shared?.endpoints).toEqual(["openrouter", "lmstudio"]);
    // The other model keeps its single endpoint.
    expect(options.find((o) => o.model === "glm-5.2")?.endpoints).toEqual([
      "openrouter",
    ]);
  });

  it("skips blank model strings (malformed endpoints.toml list entries)", () => {
    const options = buildModelOptions([ep("alpha", ["", "  ", "gpt-4"])]);
    expect(options.map((o) => o.model)).toEqual(["gpt-4"]);
  });

  it("returns an empty list for an empty endpoints array", () => {
    expect(buildModelOptions([])).toEqual([]);
  });
});

describe("filterModelOptions", () => {
  const options: ModelOption[] = [
    { model: "deepseek-v4-flash", endpoints: ["alpha"] },
    { model: "glm-5.2", endpoints: ["openrouter"] },
    { model: "glm-5.3-gcp", endpoints: ["openrouter", "lmstudio"] },
  ];

  it("an empty value shows every option", () => {
    expect(filterModelOptions(options, "")).toEqual(options);
    expect(filterModelOptions(options, "   ")).toEqual(options);
  });

  it("an exact match shows every option (reopening after a selection browses all)", () => {
    expect(filterModelOptions(options, "glm-5.2")).toEqual(options);
  });

  it("a partial value filters by case-insensitive substring", () => {
    expect(filterModelOptions(options, "glm-5").map((o) => o.model)).toEqual([
      "glm-5.2",
      "glm-5.3-gcp",
    ]);
    expect(filterModelOptions(options, "GLM").map((o) => o.model)).toEqual([
      "glm-5.2",
      "glm-5.3-gcp",
    ]);
  });

  it("free text matching nothing yields an empty list (still freely typable)", () => {
    expect(filterModelOptions(options, "not-a-configured-model")).toEqual([]);
  });
});
