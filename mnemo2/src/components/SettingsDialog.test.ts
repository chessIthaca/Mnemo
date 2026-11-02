import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { emptyModel, type EndpointDetailDto, type SettingsTransport, type TiersDto } from "../lib/settingsEngine";
import { app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import SettingsDialog from "./SettingsDialog.svelte";
import TopBar from "./TopBar.svelte";

const mocks = vi.hoisted(() => ({ transport: null as unknown, modelTransport: null as unknown, layaTransport: null as unknown }));

vi.mock("../lib/settingsEngine", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/settingsEngine")>();
  return { ...original, createSettingsTransport: () => mocks.transport };
});

vi.mock("../lib/modelSettings", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/modelSettings")>();
  return { ...original, createModelSettingsTransport: () => mocks.modelTransport };
});

vi.mock("../lib/layaSettings", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/layaSettings")>();
  return { ...original, createLayaTransport: () => mocks.layaTransport };
});

function endpoint(name: string, overrides: Partial<EndpointDetailDto> = {}): EndpointDetailDto {
  return {
    name,
    kind: "openai",
    base_url: `https://${name}.example/v1/`,
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
      { ...emptyModel("m1"), price: { input_per_1m: 3, output_per_1m: 15, cached_per_1m: 0 }, max_context: 200000 },
      emptyModel("m2"),
    ],
    ...overrides,
  };
}

function makeTransport(overrides: Partial<SettingsTransport> = {}) {
  const list = [endpoint("alpha"), endpoint("beta", { has_api_key: false })];
  const tiers: TiersDto = { difficult: ["alpha/m1"], average: ["alpha/m1", "alpha/m2"], simple: [] };
  const base = {
    endpoints: vi.fn(async () => list),
    upsertEndpoint: vi.fn(async (saved: EndpointDetailDto, _previous?: string | null) => [saved, list[1]]),
    removeEndpoint: vi.fn(async () => [list[1]]),
    setApiKey: vi.fn(async () => undefined),
    testEndpoint: vi.fn(async () => ({ ok: true, model_count: 7, error: null })),
    listProviderModels: vi.fn(async () => [
      { id: "m1", vision_capable: false, context_length: null, max_output_tokens: null, effort_levels: null },
      { id: "fresh", vision_capable: true, context_length: 128000, max_output_tokens: 8000, effort_levels: null },
    ]),
    endpointUsage: vi.fn(async () => [] as string[]),
    tiers: vi.fn(async () => tiers),
    saveTiers: vi.fn(async (next: TiersDto) => next),
  };
  const transport = { ...base, ...overrides } as typeof base;
  mocks.transport = transport;
  return transport;
}

async function openDialog() {
  render(SettingsDialog, { open: true, onclose: vi.fn() });
  await screen.findByRole("button", { name: "Edit alpha" });
}

describe("SettingsDialog", () => {
  beforeEach(() => {
    mocks.transport = null;
    mocks.modelTransport = null;
    mocks.layaTransport = null;
    app.set(fixtureState());
  });

  it("is reached from the project settings sheet opened by the gear in the top bar", async () => {
    makeTransport();
    render(TopBar);
    expect(screen.queryByRole("dialog", { name: "Settings" })).toBeNull();
    expect(screen.queryByRole("dialog", { name: "Project settings" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Project settings" }));
    expect(screen.getByRole("dialog", { name: "Project settings" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(screen.getByRole("dialog", { name: "Settings" })).toBeTruthy();
    expect(screen.queryByRole("dialog", { name: "Project settings" })).toBeNull();
    await fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Settings" })).toBeNull());
  });

  it("explains settings need the desktop app outside Tauri", () => {
    render(SettingsDialog, { open: true, onclose: vi.fn() });
    expect(screen.getByText("Settings are available in the desktop app.")).toBeTruthy();
  });

  it("lists endpoints and shows the selected editor", async () => {
    makeTransport();
    await openDialog();
    expect(screen.getAllByText("openai · 2 models")).toHaveLength(2);
    expect((screen.getByLabelText("Base URL") as HTMLInputElement).value).toBe("https://alpha.example/v1/");
    expect((screen.getByLabelText("API key") as HTMLInputElement).placeholder).toBe("saved");
    await fireEvent.click(screen.getByRole("button", { name: "Edit beta" }));
    expect((screen.getByLabelText("Base URL") as HTMLInputElement).value).toBe("https://beta.example/v1/");
    expect((screen.getByLabelText("API key") as HTMLInputElement).placeholder).toBe("");
  });

  it("shows tier membership and prices in the models table", async () => {
    makeTransport();
    await openDialog();
    expect(screen.getByText("Difficult, Average")).toBeTruthy();
    expect((screen.getByLabelText("Input price for m1") as HTMLInputElement).value).toBe("3");
  });

  it("adds an endpoint from a preset and saves without a key call when none is typed", async () => {
    const transport = makeTransport();
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Add endpoint" }));
    await fireEvent.click(screen.getByRole("button", { name: /OpenRouter/ }));
    expect((screen.getByLabelText("Base URL") as HTMLInputElement).value).toBe("https://openrouter.ai/api/v1/");
    expect((screen.getByLabelText("Endpoint name") as HTMLInputElement).value).toBe("openrouter");
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(transport.upsertEndpoint).toHaveBeenCalled());
    const [saved, previous] = transport.upsertEndpoint.mock.calls[0] as unknown as [EndpointDetailDto, string | null];
    expect(saved.name).toBe("openrouter");
    expect(saved.kind).toBe("openai");
    expect(previous).toBeNull();
    expect(transport.setApiKey).not.toHaveBeenCalled();
  });

  it("sends previousName on rename and sets the key only when typed", async () => {
    const transport = makeTransport();
    await openDialog();
    await fireEvent.input(screen.getByLabelText("Endpoint name"), { target: { value: "gamma" } });
    await fireEvent.input(screen.getByLabelText("API key"), { target: { value: "sk-new" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(transport.setApiKey).toHaveBeenCalledWith("gamma", "sk-new"));
    expect(transport.upsertEndpoint.mock.calls[0][1]).toBe("alpha");
  });

  it("shows backend save errors inline", async () => {
    makeTransport({ upsertEndpoint: vi.fn(async () => Promise.reject("endpoint name already exists")) });
    await openDialog();
    await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: "https://x.example/" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect((await screen.findByText("endpoint name already exists")).getAttribute("role")).toBe("alert");
  });

  it("asks before dropping unsaved edits when switching endpoints", async () => {
    makeTransport();
    await openDialog();
    await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: "https://changed/" } });
    await fireEvent.click(screen.getByRole("button", { name: "Edit beta" }));
    expect(screen.getByText("Discard unsaved changes?")).toBeTruthy();
    expect((screen.getByLabelText("Base URL") as HTMLInputElement).value).toBe("https://changed/");
    await fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
    expect((screen.getByLabelText("Base URL") as HTMLInputElement).value).toBe("https://beta.example/v1/");
  });

  it("tests the connection with the unsaved draft and shows the result", async () => {
    const transport = makeTransport();
    await openDialog();
    await fireEvent.input(screen.getByLabelText("API key"), { target: { value: "sk-try" } });
    await fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    expect(await screen.findByText("Connected · 7 models")).toBeTruthy();
    expect(transport.testEndpoint).toHaveBeenCalledWith("alpha", { baseUrl: "https://alpha.example/v1/", kind: "openai", apiKey: "sk-try" });
  });

  it("shows a failed connection test in the error colour text", async () => {
    makeTransport({ testEndpoint: vi.fn(async () => ({ ok: false, model_count: 0, error: "401 unauthorized" })) });
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    expect(await screen.findByText("401 unauthorized")).toBeTruthy();
  });

  it("fetches provider models and adds only the ticked ones", async () => {
    makeTransport();
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Fetch from provider" }));
    await screen.findByText("fresh");
    expect(screen.queryByLabelText("Model id m1", { selector: "ul *" })).toBeNull();
    await fireEvent.click(screen.getByRole("checkbox"));
    await fireEvent.click(screen.getByRole("button", { name: /Add 1 selected/ }));
    expect((screen.getByLabelText("Context for fresh") as HTMLInputElement).value).toBe("128000");
  });

  it("shows usage before deleting and needs a second click", async () => {
    const transport = makeTransport({ endpointUsage: vi.fn(async () => ["tier difficult: alpha/m1"]) });
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Delete endpoint" }));
    expect(await screen.findByText("tier difficult: alpha/m1")).toBeTruthy();
    expect(transport.removeEndpoint).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole("button", { name: "Confirm delete" }));
    await waitFor(() => expect(transport.removeEndpoint).toHaveBeenCalledWith("alpha"));
  });

  it("adds, reorders, removes tier models and saves", async () => {
    const transport = makeTransport();
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Tiers" }));
    const average = await screen.findByRole("region", { name: "Average tier" });
    expect(within(average).getByText("Default")).toBeTruthy();
    await fireEvent.click(within(average).getByRole("button", { name: "Move alpha/m2 up in Average" }));
    await fireEvent.click(within(average).getByRole("button", { name: "Remove alpha/m1 from Average" }));
    const simple = screen.getByRole("region", { name: "Simple and tools tier" });
    await fireEvent.change(within(simple).getByLabelText("Add model to Simple and tools"), { target: { value: "beta/m1" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save tiers" }));
    await waitFor(() => expect(transport.saveTiers).toHaveBeenCalledWith({ difficult: ["alpha/m1"], average: ["alpha/m2"], simple: ["beta/m1"] }));
  });

  it("shows tier validation errors", async () => {
    makeTransport({ saveTiers: vi.fn(async () => Promise.reject("unknown model x/y")) });
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Tiers" }));
    const simple = await screen.findByRole("region", { name: "Simple and tools tier" });
    await fireEvent.change(within(simple).getByLabelText("Add model to Simple and tools"), { target: { value: "alpha/m1" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save tiers" }));
    expect(await screen.findByText("unknown model x/y")).toBeTruthy();
  });

  it("shows the Models and Savings pages beside Endpoints and Tiers", async () => {
    makeTransport();
    mocks.modelTransport = {
      get: vi.fn(async () => ({
        task_models: { planning: "alpha/m1", executing: null, bug_fixing: null, reviewing: null, summarize: null },
        vision_model: null,
        embedding: {
          selected: "hash",
          status: "keyword_only",
          options: [{ id: "hash", label: "Keyword only", size_mb: 0, installed: true }],
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
          compress_extra_commands: [],
        },
        pricing: [{ model: "m1", input_per_1m: 3, output_per_1m: 15, cached_per_1m: 0 }],
        budget: { enabled: false, max_tokens_per_plan: 0, max_escalations_per_plan: 0, max_retries_per_lane: 0 },
      })),
      save: vi.fn(async () => undefined),
      install: vi.fn(async () => undefined),
      onEmbedderEvent: vi.fn(async () => () => {}),
    };
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Models" }));
    const work = await screen.findByRole("region", { name: "Models for each kind of work" });
    expect(within(work).getByLabelText("Planning")).toBeTruthy();
    expect(screen.getByText("Status: Keyword only")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Savings" }));
    const levers = await screen.findByRole("region", { name: "Savings levers" });
    expect(within(levers).getByText("Delta reads")).toBeTruthy();
    expect((screen.getByLabelText("Model for row 1") as HTMLInputElement).value).toBe("m1");
  });

  it("shows the Decisions page beside the others, with status, switches and lanes", async () => {
    makeTransport();
    mocks.layaTransport = {
      status: vi.fn(async () => ({
        enabled: true,
        installed: true,
        status: "ready",
        flags: {
          auto_type_memories: true,
          failure_triage: false,
          failure_triage_knn: false,
          auto_finetune: false,
          steer_tool_choice: false,
          routing: false,
          reflex: false,
        },
        routing: {
          cheap: "alpha/m1",
          capable: "alpha/m2",
          lane_medium: null,
          lane_high: null,
          escalate: null,
          threshold: 0.8,
          enforce: false,
        },
      })),
      setup: vi.fn(async () => undefined),
      disable: vi.fn(async () => undefined),
      save: vi.fn(async () => undefined),
      onLayaEvent: vi.fn(async () => () => {}),
    };
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Decisions" }));
    const status = await screen.findByRole("region", { name: "Decision models (Laya)" });
    expect(within(status).getByText("Status: Ready")).toBeTruthy();
    expect(within(status).getByRole("button", { name: "Disable" })).toBeTruthy();
    expect((screen.getByLabelText("Cheap lane") as HTMLInputElement).value).toBe("alpha/m1");
    expect((screen.getByRole("checkbox", { name: /Auto-type memories/ }) as HTMLInputElement).checked).toBe(true);
  });

  it("explains decision settings need the desktop app without a Laya transport", async () => {
    makeTransport();
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Decisions" }));
    expect(await screen.findByText("Decision settings are available in the desktop app.")).toBeTruthy();
  });
});
