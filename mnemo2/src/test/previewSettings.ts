import {
  EMBEDDING_KEYWORD_ID,
  setPreviewModelSettingsTransport,
  type EmbedderEvent,
  type ModelSettings,
  type ModelSettingsSection,
  type ModelSettingsTransport,
  type OptimizerSettings,
  type PricingRow,
  type TaskModels,
} from "../lib/modelSettings";
import {
  emptyModel,
  setPreviewSettingsTransport,
  type EndpointDetailDto,
  type SettingsTransport,
  type TiersDto,
} from "../lib/settingsEngine";

/**
 * Dev-only seed for the `settings-models` preview scene: the sample data
 * the settings pages render with, plus in-memory fakes of the two IPC
 * transports (no Tauri host in a browser). This file lives under src/test/
 * so the no-mock guard keeps it out of the shipped app; the scene mounts
 * the real SettingsDialog against these fakes.
 */

// ── Sample config ────────────────────────────────────────────────────────────

function model(id: string, overrides: Partial<EndpointDetailDto["models"][number]> = {}): EndpointDetailDto["models"][number] {
  return { ...emptyModel(id), ...overrides };
}

const previewEndpoints: EndpointDetailDto[] = [
  {
    name: "openrouter",
    kind: "openrouter",
    base_url: "https://openrouter.ai/api/v1/",
    has_api_key: true,
    max_context: null,
    max_output_tokens: null,
    multimodal: false,
    supports_reasoning_effort: true,
    reasoning_effort: null,
    supports_strict_schema: null,
    workspace_id: null,
    temperature: null,
    models: [
      model("glm-4.6-flash", { multimodal: false, price: { input_per_1m: 0.12, output_per_1m: 0.39, cached_per_1m: 0.012 } }),
      model("glm-5.3-flash", { multimodal: true, price: { input_per_1m: 0.25, output_per_1m: 0.88, cached_per_1m: 0.03 } }),
      model("gpt-5.2", { multimodal: true, price: { input_per_1m: 1.25, output_per_1m: 10, cached_per_1m: 0.125 } }),
    ],
  },
  {
    name: "lmstudio",
    kind: "openai",
    base_url: "http://127.0.0.1:1234/v1/",
    has_api_key: false,
    max_context: null,
    max_output_tokens: null,
    multimodal: false,
    supports_reasoning_effort: false,
    reasoning_effort: null,
    supports_strict_schema: null,
    workspace_id: null,
    temperature: null,
    models: [
      model("qwen3-30b-a3b", { price: { input_per_1m: 0.05, output_per_1m: 0.2, cached_per_1m: 0 } }),
      model("qwen3-coder-30b", { price: null }),
    ],
  },
];

const previewTiers: TiersDto = {
  tiers: [
    { id: "trivial", name: "Trivial", definition: "Single-file edits, checks, lookups, scaffolding", models: ["openrouter/glm-4.6-flash"], effort: null, role: null, resolves_to: "trivial" },
    { id: "routine", name: "Routine", definition: "Typical implementation steps", models: ["openrouter/glm-5.3-flash"], effort: null, role: null, resolves_to: "trivial" },
    { id: "medium", name: "Medium", definition: "Multi-file change with a clear spec", models: [], effort: null, role: null, resolves_to: "routine" },
    { id: "high", name: "High", definition: "Design, debugging, bug fixes, shared contracts", models: ["openrouter/gpt-5.2"], effort: null, role: null, resolves_to: "medium" },
    { id: "escalate", name: "Escalate", definition: "Only after a lower tier failed", models: [], effort: null, role: "escalate", resolves_to: "high" },
  ],
};

function previewSettings(): ModelSettings {
  return {
    task_models: {
      summarize: null,
    },
    vision_model: "openrouter/gpt-5.2",
    embedding: {
      selected: "all-MiniLM-L6-v2",
      status: "missing",
      options: [
        { id: "bge-small-en-v1.5", label: "BGE small en v1.5", size_mb: 133, installed: true },
        { id: "all-MiniLM-L6-v2", label: "MiniLM — default", size_mb: 90, installed: false },
        { id: "nomic-embed-text-v1.5", label: "Nomic embed text v1.5", size_mb: 273, installed: false },
        { id: EMBEDDING_KEYWORD_ID, label: "Keyword only", size_mb: 0, installed: true },
      ],
    },
    optimizer: {
      delta_reads: true,
      compress_output: true,
      archive: true,
      compaction_survival: true,
      quality_score: true,
      lean_output_nudge: true,
      recall_delta: false,
      archive_min_chars: 2400,
      compress_min_chars: 3000,
      lean_output_fill_pct: 80,
      nudge_cooldown_requests: 4,
      compress_extra_commands: ["cargo test", "npm run build"],
    },
    pricing: [
      { model: "glm-4.6-flash", input_per_1m: 0.12, output_per_1m: 0.39, cached_per_1m: 0.012 },
      { model: "glm-5.3-flash", input_per_1m: 0.25, output_per_1m: 0.88, cached_per_1m: 0.03 },
      { model: "gpt-5.2", input_per_1m: 1.25, output_per_1m: 10, cached_per_1m: 0.125 },
      { model: "qwen3-30b-a3b", input_per_1m: 0.05, output_per_1m: 0.2, cached_per_1m: 0 },
    ],
    budget: { enabled: true, max_tokens_per_plan: 2_000_000, max_escalations_per_plan: 3, max_retries_per_lane: 2 },
  };
}

// ── Fakes ─────────────────────────────────────────────────────────────────────

/** The settings transport of the preview: reading works, writing refuses politely. */
function createPreviewSettingsTransport(): SettingsTransport {
  return {
    endpoints: async () => previewEndpoints.map((endpoint) => ({ ...endpoint, models: [...endpoint.models] })),
    upsertEndpoint: () => Promise.reject("Endpoints are read-only in the preview"),
    removeEndpoint: () => Promise.reject("Endpoints are read-only in the preview"),
    setApiKey: () => Promise.reject("Endpoints are read-only in the preview"),
    testEndpoint: () => Promise.reject("No connection from the preview"),
    listProviderModels: () => Promise.reject("No connection from the preview"),
    endpointUsage: async () => [],
    tiers: async () => previewTiers,
    saveTiers: () => Promise.reject("Tiers are read-only in the preview"),
  };
}

/** The status a selection would have with the fake install cache. */
function statusOf(settings: ModelSettings): ModelSettings["embedding"]["status"] {
  const choice = settings.embedding.options.find((option) => option.id === settings.embedding.selected);
  if (!choice || choice.id === EMBEDDING_KEYWORD_ID) return "keyword_only";
  return choice.installed ? "ready" : "missing";
}

/**
 * The model-settings transport of the preview: an in-memory config that
 * honors every section save, and an install that streams staggered
 * progress events so the progress bar and status flip are visible.
 */
export function createPreviewModelSettingsTransport(): ModelSettingsTransport {
  const settings = previewSettings();
  let handlers: ((event: EmbedderEvent) => void)[] = [];

  const emit = (event: EmbedderEvent) => {
    for (const handler of [...handlers]) handler(event);
  };

  const get = async (): Promise<ModelSettings> => structuredClone(settings);

  const save = async (section: ModelSettingsSection, value: unknown): Promise<void> => {
    switch (section) {
      case "models":
        settings.task_models = { ...settings.task_models, ...(value as Partial<TaskModels>) };
        break;
      case "vision":
        settings.vision_model = (value as string | null) ?? null;
        break;
      case "embedding":
        settings.embedding.selected = value as string;
        settings.embedding.status = statusOf(settings);
        break;
      case "optimizer":
        settings.optimizer = { ...settings.optimizer, ...(value as Partial<OptimizerSettings>) };
        break;
      case "pricing":
        settings.pricing = value as PricingRow[];
        break;
      case "budget":
        settings.budget = { ...settings.budget, ...(value as Partial<ModelSettings["budget"]>) };
        break;
    }
  };

  const install = async (modelId: string): Promise<void> => {
    const choice = settings.embedding.options.find((option) => option.id === modelId);
    if (!choice || modelId === EMBEDDING_KEYWORD_ID) throw "No such bundled model";
    const steps: [number, number][] = [
      [150, 0.08],
      [600, 0.35],
      [1100, 0.62],
      [1600, 0.9],
    ];
    for (const [delay, fraction] of steps) {
      setTimeout(() => {
        emit({ type: "progress", step: "embedder_model", label: `Downloading ${choice.label}`, fraction });
      }, delay);
    }
    setTimeout(() => {
      choice.installed = true;
      if (settings.embedding.selected === modelId) settings.embedding.status = "ready";
      emit({ type: "done", step: "embedder_model" });
    }, 2100);
  };

  return {
    get,
    save,
    install,
    onEmbedderEvent: async (handler) => {
      handlers.push(handler);
      return () => {
        handlers = handlers.filter((entry) => entry !== handler);
      };
    },
  };
}

/**
 * Wire both preview fakes so the settings dialog renders against the seed.
 * Returns a cleanup that restores the browser-only behavior.
 */
export function seedPreviewSettings(): () => void {
  setPreviewSettingsTransport(createPreviewSettingsTransport());
  setPreviewModelSettingsTransport(createPreviewModelSettingsTransport());
  return () => {
    setPreviewSettingsTransport(null);
    setPreviewModelSettingsTransport(null);
  };
}