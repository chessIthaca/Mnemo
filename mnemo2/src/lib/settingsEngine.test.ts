import { describe, expect, it, vi } from "vitest";
import {
  createSettingsTransport,
  createTauriSettingsTransport,
  emptyEndpoint,
  emptyModel,
  type EndpointDetailDto,
} from "./settingsEngine";

function transport() {
  const invoke = vi.fn(async (_command: string, _args?: unknown) => [] as unknown);
  return { invoke, transport: createTauriSettingsTransport({ invoke: invoke as never }) };
}

describe("settingsEngine", () => {
  it("reads endpoints and tiers", async () => {
    const { invoke, transport: t } = transport();
    await t.endpoints();
    await t.tiers();
    expect(invoke).toHaveBeenNthCalledWith(1, "settings_endpoints");
    expect(invoke).toHaveBeenNthCalledWith(2, "settings_tiers");
  });

  it("upserts with an optional rename and removes by name", async () => {
    const { invoke, transport: t } = transport();
    const endpoint: EndpointDetailDto = {
      ...emptyEndpoint(),
      name: "alpha",
      base_url: "http://h/",
      models: [emptyModel("m1")],
    };
    await t.upsertEndpoint(endpoint);
    await t.upsertEndpoint(endpoint, "a");
    await t.removeEndpoint("alpha");
    expect(invoke).toHaveBeenNthCalledWith(1, "settings_upsert_endpoint", { endpoint, previousName: null });
    expect(invoke).toHaveBeenNthCalledWith(2, "settings_upsert_endpoint", { endpoint, previousName: "a" });
    expect(invoke).toHaveBeenNthCalledWith(3, "settings_remove_endpoint", { name: "alpha" });
  });

  it("sets keys and probes with explicit or saved credentials", async () => {
    const { invoke, transport: t } = transport();
    await t.setApiKey("alpha", "sk-1");
    await t.testEndpoint("alpha");
    await t.listProviderModels("alpha", { baseUrl: "http://h/", apiKey: "k", kind: "openai" });
    expect(invoke).toHaveBeenNthCalledWith(1, "settings_set_api_key", { endpointName: "alpha", apiKey: "sk-1" });
    expect(invoke).toHaveBeenNthCalledWith(2, "settings_test_endpoint", {
      endpointName: "alpha",
      baseUrl: null,
      apiKey: null,
      kind: null,
    });
    expect(invoke).toHaveBeenNthCalledWith(3, "settings_list_provider_models", {
      endpointName: "alpha",
      baseUrl: "http://h/",
      apiKey: "k",
      kind: "openai",
    });
  });

  it("asks for usage and saves tiers", async () => {
    const { invoke, transport: t } = transport();
    const tiers = {
      tiers: [
        { id: "trivial", name: "Trivial", definition: "Lookups", models: ["a/m2"], effort: null, role: null, resolves_to: "trivial" },
        { id: "routine", name: "Routine", definition: "Steps", models: [], effort: null, role: null, resolves_to: "trivial" },
        { id: "medium", name: "Medium", definition: "Features", models: ["a/m1"], effort: null, role: null, resolves_to: "routine" },
        { id: "high", name: "High", definition: "Design", models: [], effort: null, role: null, resolves_to: "medium" },
        { id: "escalate", name: "Escalate", definition: "After a failure", models: [], effort: null, role: "escalate", resolves_to: "high" },
      ],
    };
    await t.endpointUsage("a");
    await t.saveTiers(tiers);
    expect(invoke).toHaveBeenNthCalledWith(1, "settings_endpoint_usage", { name: "a" });
    expect(invoke).toHaveBeenNthCalledWith(2, "settings_save_tiers", { tiers });
  });

  it("has no transport outside the native shell", () => {
    expect(createSettingsTransport()).toBeNull();
  });
});
