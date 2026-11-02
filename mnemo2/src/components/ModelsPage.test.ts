import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EmbedderEvent, ModelSettings, ModelSettingsSection, ModelSettingsTransport, TaskModels } from "../lib/modelSettings";
import { emptyModel, type EndpointDetailDto } from "../lib/settingsEngine";
import ModelsPage from "./ModelsPage.svelte";

/**
 * ModelsPage — the per-task, vision, code-indexing and budget sections of
 * the Models settings page, each with its own save button. The transport is
 * a prop, so the tests hand the page a fake directly.
 */

function sampleSettings(): ModelSettings {
  return {
    task_models: {
      planning: "openrouter/glm-5.3-flash",
      executing: null,
      bug_fixing: null,
      reviewing: "openrouter/gpt-5.2",
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
        { id: "hash", label: "Keyword only", size_mb: 0, installed: true },
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
      compress_extra_commands: ["cargo test"],
    },
    pricing: [
      { model: "glm-4.6-flash", input_per_1m: 0.12, output_per_1m: 0.39, cached_per_1m: 0.012 },
      { model: "gpt-5.2", input_per_1m: 1.25, output_per_1m: 10, cached_per_1m: 0.125 },
    ],
    budget: { enabled: true, max_tokens_per_plan: 2_000_000, max_escalations_per_plan: 3, max_retries_per_lane: 2 },
  };
}

function sampleEndpoints(): EndpointDetailDto[] {
  return [
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
        { ...emptyModel("glm-4.6-flash"), price: { input_per_1m: 0.12, output_per_1m: 0.39, cached_per_1m: 0.012 } },
        emptyModel("glm-5.3-flash"),
        emptyModel("gpt-5.2"),
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
      models: [emptyModel("qwen3-30b-a3b")],
    },
  ];
}

function makeTransport(overrides: Partial<ModelSettingsTransport> = {}) {
  const settings = sampleSettings();
  const handlers: ((event: EmbedderEvent) => void)[] = [];
  const base = {
    get: vi.fn(async () => structuredClone(settings)),
    save: vi.fn(async (section: ModelSettingsSection, value: unknown) => undefined),
    install: vi.fn(async () => undefined),
    onEmbedderEvent: vi.fn(async (handler: (event: EmbedderEvent) => void) => {
      handlers.push(handler);
      return () => {};
    }),
  };
  return { settings, handlers, transport: { ...base, ...overrides } as typeof base };
}

function page(overrides: Partial<ModelSettingsTransport> = {}) {
  const made = makeTransport(overrides);
  render(ModelsPage, { props: { transport: made.transport, endpoints: sampleEndpoints() } });
  return made;
}

describe("ModelsPage", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders the task slots, vision, code-indexing list and budget from the snapshot", async () => {
    page();
    expect((await screen.findByLabelText("Planning") as HTMLInputElement).value).toBe("openrouter/glm-5.3-flash");
    expect((screen.getByLabelText("Executing") as HTMLInputElement).value).toBe("");
    expect((screen.getByLabelText("Vision model") as HTMLInputElement).value).toBe("openrouter/gpt-5.2");
    expect((screen.getByLabelText("Tokens per plan") as HTMLInputElement).value).toBe("2000000");
    expect(screen.getByRole("radio", { name: "MiniLM — default 90 MB" })).toBeTruthy();
    expect(screen.getByText("Installed")).toBeTruthy();
    expect(screen.getByText("Keyword only")).toBeTruthy();
    expect(screen.getByText("Status: Not installed")).toBeTruthy();
  });

  it("saves task models and shows the reopen note", async () => {
    const made = page();
    await screen.findByLabelText("Planning");
    await fireEvent.input(screen.getByLabelText("Planning"), { target: { value: "lmstudio/qwen3-30b-a3b" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save models" }));
    await waitFor(() => expect(made.transport.save).toHaveBeenCalledWith("models", {
      planning: "lmstudio/qwen3-30b-a3b",
      executing: null,
      bug_fixing: null,
      reviewing: "openrouter/gpt-5.2",
      summarize: null,
    }));
    expect(await screen.findByText("Applies after reopening the project.")).toBeTruthy();
  });

  it("sends null for a cleared slot", async () => {
    const made = page();
    await screen.findByLabelText("Planning");
    await fireEvent.click(screen.getByRole("button", { name: "Use the default for Planning" }));
    await fireEvent.click(screen.getByRole("button", { name: "Save models" }));
    await waitFor(() => expect(made.transport.save).toHaveBeenCalled());
    const payload = made.transport.save.mock.calls[0][1] as TaskModels;
    expect(payload.planning).toBeNull();
  });

  it("rejects a malformed model ref inline without saving", async () => {
    const made = page();
    await screen.findByLabelText("Planning");
    await fireEvent.input(screen.getByLabelText("Planning"), { target: { value: "no-slash" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save models" }));
    expect(screen.getByText('Planning model must be "endpoint/model"')).toBeTruthy();
    expect(made.transport.save).not.toHaveBeenCalled();
  });

  it("saves the vision model and clears it to null", async () => {
    const made = page();
    await screen.findByLabelText("Vision model");
    await fireEvent.input(screen.getByLabelText("Vision model"), { target: { value: "lmstudio/qwen3-30b-a3b" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save vision" }));
    await waitFor(() => expect(made.transport.save).toHaveBeenCalledWith("vision", "lmstudio/qwen3-30b-a3b"));
    await fireEvent.click(screen.getByRole("button", { name: "Use the default for Vision" }));
    await fireEvent.click(screen.getByRole("button", { name: "Save vision" }));
    await waitFor(() => expect(made.transport.save).toHaveBeenCalledWith("vision", null));
  });

  it("saves the code-indexing selection and names the reopen note", async () => {
    const made = page();
    await screen.findByText("Code indexing");
    await fireEvent.click(screen.getByRole("radio", { name: "Nomic embed text v1.5 273 MB" }));
    await fireEvent.click(screen.getByRole("button", { name: "Save indexing" }));
    await waitFor(() => expect(made.transport.save).toHaveBeenCalledWith("embedding", "nomic-embed-text-v1.5"));
    expect(await screen.findByText(/a fresh download is used from the next reopen/)).toBeTruthy();
  });

  it("installs the drafted model, streams the progress bar and turns the status Ready", async () => {
    const made = page();
    await screen.findByText("Status: Not installed");
    await fireEvent.click(screen.getByRole("button", { name: "Install" }));
    expect(made.transport.install).toHaveBeenCalledWith("all-MiniLM-L6-v2");
    made.handlers[0]({ type: "progress", step: "embedder_model", label: "Downloading MiniLM", fraction: 0.62 });
    const bar = await screen.findByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBe("62");
    expect(screen.getByText("Downloading MiniLM")).toBeTruthy();
    made.settings.embedding.status = "ready";
    made.settings.embedding.options[1].installed = true;
    made.handlers[0]({ type: "done", step: "embedder_model" });
    await screen.findByText("Status: Ready");
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("keeps an unsaved task edit across the install refresh", async () => {
    const made = page();
    await screen.findByLabelText("Planning");
    await fireEvent.input(screen.getByLabelText("Planning"), { target: { value: "lmstudio/qwen3-30b-a3b" } });
    made.settings.embedding.status = "ready";
    made.settings.embedding.options[1].installed = true;
    made.handlers[0]({ type: "done", step: "embedder_model" });
    await screen.findByText("Status: Ready");
    expect((screen.getByLabelText("Planning") as HTMLInputElement).value).toBe("lmstudio/qwen3-30b-a3b");
  });

  it("shows a failed install inline", async () => {
    const made = page();
    await screen.findByText("Status: Not installed");
    made.handlers[0]({ type: "failed", step: "embedder_model", error: "disk full" });
    expect(await screen.findByText("disk full")).toBeTruthy();
  });

  it("saves budget caps with the enabled toggle", async () => {
    const made = page();
    await screen.findByLabelText("Tokens per plan");
    await fireEvent.click(screen.getByLabelText("Enforce budget caps"));
    await fireEvent.input(screen.getByLabelText("Tokens per plan"), { target: { value: "1500000" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save budget" }));
    await waitFor(() =>
      expect(made.transport.save).toHaveBeenCalledWith("budget", {
        enabled: false,
        max_tokens_per_plan: 1_500_000,
        max_escalations_per_plan: 3,
        max_retries_per_lane: 2,
      }),
    );
  });

  it("rejects an invalid cap inline without saving", async () => {
    const made = page();
    await screen.findByLabelText("Tokens per plan");
    await fireEvent.input(screen.getByLabelText("Tokens per plan"), { target: { value: "lots" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save budget" }));
    expect(screen.getByText("Tokens per plan must be a whole number")).toBeTruthy();
    expect(made.transport.save).not.toHaveBeenCalled();
  });

  it("shows the load error when the snapshot fails", async () => {
    page({ get: vi.fn(async () => Promise.reject("config unreadable")) });
    expect(await screen.findByText("config unreadable")).toBeTruthy();
  });
});