import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

/** USD per 1M tokens. */
export interface PriceDto {
  input_per_1m: number;
  output_per_1m: number;
  cached_per_1m: number;
}

/** One model of an endpoint. Wire fields are snake_case (serde). */
export interface ModelDetailDto {
  id: string;
  /** Input only: the id before a rename, so tiers follow it. */
  previous_id?: string | null;
  max_context: number | null;
  max_output_tokens: number | null;
  /** Allowed reasoning efforts; empty = the endpoint defaults. */
  reasoning_efforts: string[];
  reasoning_effort: string | null;
  reasoning_effort_off_wire: string | null;
  /** null = inherit the endpoint multimodal flag. */
  multimodal: boolean | null;
  temperature: number | null;
  price: PriceDto | null;
}

export interface EndpointDetailDto {
  name: string;
  /** "openai" | "local" | "anthropic". */
  kind: string;
  base_url: string;
  /** Output only. */
  has_api_key: boolean;
  max_context: number | null;
  max_output_tokens: number | null;
  multimodal: boolean;
  supports_reasoning_effort: boolean;
  reasoning_effort: string | null;
  /** null = auto. */
  supports_strict_schema: boolean | null;
  /** Anthropic only. */
  workspace_id: string | null;
  temperature: number | null;
  models: ModelDetailDto[];
}

export type TierName = "difficult" | "average" | "simple";
export const TIER_NAMES: readonly TierName[] = ["difficult", "average", "simple"];

/** Ordered `endpoint/model` specs per tier; the first is the tier default. */
export interface TiersDto {
  difficult: string[];
  average: string[];
  simple: string[];
}

export interface EndpointTestDto {
  ok: boolean;
  model_count: number;
  error: string | null;
}

export interface ProviderModelDto {
  id: string;
  vision_capable: boolean;
  context_length: number | null;
  max_output_tokens: number | null;
  effort_levels: string[] | null;
}

/** Optional credentials for a probe; omitted fields fall back to the saved endpoint. */
export interface ProbeOptions {
  baseUrl?: string | null;
  apiKey?: string | null;
  kind?: string | null;
}

export interface SettingsTransportBindings {
  invoke: typeof invoke;
}

export interface SettingsTransport {
  endpoints(): Promise<EndpointDetailDto[]>;
  /** Create or update; `previousName` renames. Resolves to the refreshed list. */
  upsertEndpoint(endpoint: EndpointDetailDto, previousName?: string | null): Promise<EndpointDetailDto[]>;
  /** Resolves to the refreshed list. */
  removeEndpoint(name: string): Promise<EndpointDetailDto[]>;
  setApiKey(endpointName: string, apiKey: string): Promise<void>;
  testEndpoint(endpointName: string, options?: ProbeOptions): Promise<EndpointTestDto>;
  listProviderModels(endpointName: string, options?: ProbeOptions): Promise<ProviderModelDto[]>;
  /** What references the endpoint, e.g. "tier difficult: a/m1" or "default model: a/m1". */
  endpointUsage(name: string): Promise<string[]>;
  tiers(): Promise<TiersDto>;
  /** Resolves to the stored (trimmed, de-duplicated) tiers; rejects unknown specs. */
  saveTiers(tiers: TiersDto): Promise<TiersDto>;
}

function probeArgs(endpointName: string, options?: ProbeOptions) {
  return {
    endpointName,
    baseUrl: options?.baseUrl ?? null,
    apiKey: options?.apiKey ?? null,
    kind: options?.kind ?? null,
  };
}

export function createTauriSettingsTransport(bindings: SettingsTransportBindings): SettingsTransport {
  return {
    endpoints: () => bindings.invoke<EndpointDetailDto[]>("settings_endpoints"),
    upsertEndpoint: (endpoint, previousName) =>
      bindings.invoke<EndpointDetailDto[]>("settings_upsert_endpoint", {
        endpoint,
        previousName: previousName ?? null,
      }),
    removeEndpoint: (name) => bindings.invoke<EndpointDetailDto[]>("settings_remove_endpoint", { name }),
    setApiKey: (endpointName, apiKey) =>
      bindings.invoke<void>("settings_set_api_key", { endpointName, apiKey }),
    testEndpoint: (endpointName, options) =>
      bindings.invoke<EndpointTestDto>("settings_test_endpoint", probeArgs(endpointName, options)),
    listProviderModels: (endpointName, options) =>
      bindings.invoke<ProviderModelDto[]>("settings_list_provider_models", probeArgs(endpointName, options)),
    endpointUsage: (name) => bindings.invoke<string[]>("settings_endpoint_usage", { name }),
    tiers: () => bindings.invoke<TiersDto>("settings_tiers"),
    saveTiers: (tiers) => bindings.invoke<TiersDto>("settings_save_tiers", { tiers }),
  };
}

/**
 * Dev-only seam: the preview page (no Tauri host) seeds a fake transport
 * here before mounting the settings dialog, so it renders sample data in a
 * browser. Nothing in the shipped app calls the setter, and the browser
 * check below still returns null when no seed is set.
 */
let previewTransport: SettingsTransport | null = null;

/** Seed (or clear) the preview settings transport — dev preview only. */
export function setPreviewSettingsTransport(transport: SettingsTransport | null): void {
  previewTransport = transport;
}

/** The native settings transport, or `null` in a browser-only session. */
export function createSettingsTransport(bindings?: SettingsTransportBindings): SettingsTransport | null {
  if (previewTransport) return previewTransport;
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriSettingsTransport(bindings ?? { invoke });
}

/** A blank model row for the endpoint editor. */
export function emptyModel(id = ""): ModelDetailDto {
  return {
    id,
    max_context: null,
    max_output_tokens: null,
    reasoning_efforts: [],
    reasoning_effort: null,
    reasoning_effort_off_wire: null,
    multimodal: null,
    temperature: null,
    price: null,
  };
}

/** A blank endpoint for the add-endpoint form. */
export function emptyEndpoint(): EndpointDetailDto {
  return {
    name: "",
    kind: "openai",
    base_url: "",
    has_api_key: false,
    max_context: null,
    max_output_tokens: null,
    multimodal: false,
    supports_reasoning_effort: true,
    reasoning_effort: null,
    supports_strict_schema: null,
    workspace_id: null,
    temperature: null,
    models: [],
  };
}
