import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { emptyModel, emptyTiers, type EndpointDetailDto, type SettingsTransport, type TiersDto } from "../lib/settingsEngine";
import { setPreviewAppearanceTransport, type AppearanceSettings } from "../lib/sounds";
import { app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import SettingsDialog from "./SettingsDialog.svelte";
import EndpointsPage from "./EndpointsPage.svelte";
import TopBar from "./TopBar.svelte";


const mocks = vi.hoisted(() => ({
  transport: null as unknown,
  modelTransport: null as unknown,
  layaTransport: null as unknown,
  safetyTransport: null as unknown,
  mcpTransport: null as unknown,
  specsTransport: null as unknown,
  aboutTransport: null as unknown,
}));

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

vi.mock("../lib/safetySettings", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/safetySettings")>();
  return { ...original, createSafetyTransport: () => mocks.safetyTransport };
});

vi.mock("../lib/mcpSettings", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/mcpSettings")>();
  return { ...original, createMcpTransport: () => mocks.mcpTransport };
});

vi.mock("../lib/specsSettings", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/specsSettings")>();
  return { ...original, createSpecsTransport: () => mocks.specsTransport };
});

vi.mock("../lib/about", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/about")>();
  return { ...original, createAboutTransport: () => mocks.aboutTransport };
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
  /** The loaded ladder: trivial keeps its required model; medium and escalate are empty. */
  const tiers: TiersDto = {
    tiers: [
      { id: "trivial", name: "Trivial", definition: "Single-file edits, checks, lookups, scaffolding", models: ["alpha/m1"], effort: null, role: null, resolves_to: "trivial" },
      { id: "routine", name: "Routine", definition: "Typical implementation steps", models: ["alpha/m1", "alpha/m2"], effort: null, role: null, resolves_to: "trivial" },
      { id: "medium", name: "Medium", definition: "Multi-file change with a clear spec", models: [], effort: null, role: null, resolves_to: "routine" },
      { id: "high", name: "High", definition: "Design, debugging, bug fixes, shared contracts", models: ["alpha/m1"], effort: null, role: null, resolves_to: "medium" },
      { id: "escalate", name: "Escalate", definition: "Only after a lower tier failed", models: [], effort: null, role: "escalate", resolves_to: "high" },
    ],
  };
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

function safetySnapshot() {
  return { mode: "approve-each-action", rules: [], project_open: true, live_lead: false };
}

function makeSafetyTransport() {
  mocks.safetyTransport = {
    get: vi.fn(async () => safetySnapshot()),
    saveMode: vi.fn(async () => safetySnapshot()),
    saveRules: vi.fn(async () => safetySnapshot()),
  };
}

function mcpServer() {
  return {
    name: "fs",
    enabled: true,
    command: "npx",
    args: ["-y", "server-fs"],
    env: [],
    headers_env: {},
    scopes: [],
    trusted: false,
    source: "global",
  };
}

function makeMcpTransport() {
  const snapshot = { servers: [mcpServer()], project_open: true, live_lead: false, statuses: [] };
  mocks.mcpTransport = {
    get: vi.fn(async () => snapshot),
    save: vi.fn(async () => snapshot),
    test: vi.fn(async () => ({ ok: true, tools: [], error: null })),
  };
}

/** A ready Laya transport whose routing wire carries NO `effort` key —
 * exactly what the real backend sends. The old test fixtures added one,
 * which is what hid the page's false save prompt for so long. */
function makeLayaTransport() {
  const control = {
    choice: "suggest",
    laya_routing: true,
    laya_status: "ready",
    checkpoint: "Laya (English)",
    knn_rows: 42,
    knn_min_rows: 30,
  };
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
        routing: true,
        reflex: false,
      },
      routing: { threshold: 0.8, first_prompt_min_confidence: null, enforce: false },
    })),
    topControl: vi.fn(async () => control),
    routingStatus: vi.fn(async () => ({
      settings: {
        mode: "suggest",
        min_confidence_4: 0.4,
        min_confidence_3: 0.475,
        min_confidence_2: 0.6,
        knn_min_share: 0.8,
        knn_min_rows: 30,
        fallback_tier: "high",
        nudges: true,
        min_words: 20,
      },
      summary: {
        decisions_by_mode: { shadow: 31, suggest: 8 },
        agreement_rate: null,
        laya_accuracy: null,
        lead_accuracy: null,
        labelled: 42,
        overrides_by_source: {},
        nudges: 0,
        nudges_acted_on: 0,
        eval_date: null,
      },
      eval: null,
    })),
    setup: vi.fn(async () => undefined),
    disable: vi.fn(async () => undefined),
    saveFlag: vi.fn(async () => undefined),
    checkLaya: vi.fn(async () => undefined),
    saveTopControl: vi.fn(async () => control),
    onLayaEvent: vi.fn(async () => () => {}),
  };
  return mocks.layaTransport;
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
    mocks.safetyTransport = null;
    mocks.mcpTransport = null;
    mocks.specsTransport = null;
    mocks.aboutTransport = null;
    app.set(fixtureState());
  });


  it("opens from the gear straight onto the Project page, with no per-project sheet left", async () => {
    makeTransport();
    render(TopBar);
    expect(screen.queryByRole("dialog", { name: "Settings" })).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    const dialog = await screen.findByRole("dialog", { name: "Settings" });
    // The gear lands on the Project page: slots, lead model, merge policy.
    expect(within(dialog).getByLabelText("Project worker slots")).toBeTruthy();
    expect(within(dialog).getByText("Lead model")).toBeTruthy();
    expect(within(dialog).getByText("Guarded")).toBeTruthy();
    // The rail files the open project under "This project", the rest under
    // "All projects", and names the project by its own name.
    expect(within(dialog).getByText("This project")).toBeTruthy();
    expect(within(dialog).getByText("All projects")).toBeTruthy();
    expect(within(dialog).getByRole("button", { name: "Mnemo" })).toBeTruthy();
    expect(screen.queryByRole("dialog", { name: "Project settings" })).toBeNull();

    await fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Settings" })).toBeNull());
  });

  it("opens on Endpoints when no project is open, and the Project page says so", async () => {
    makeTransport();
    app.set({ ...fixtureState(), projectId: null });
    render(SettingsDialog, { open: true, onclose: vi.fn(), initialPage: "project" });

    // There is no project to configure, so the dialog falls back to Endpoints.
    expect(await screen.findByRole("button", { name: "Edit alpha" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Endpoints" }).getAttribute("aria-current")).toBe("page");

    await fireEvent.click(screen.getByRole("button", { name: "Project" }));
    expect(screen.getByText("Open a project to change its settings.")).toBeTruthy();
  });

  it("opens directly on the page a caller asks for", async () => {
    makeTransport();
    render(SettingsDialog, { open: true, onclose: vi.fn(), initialPage: "tiers" });

    const tiers = await screen.findByRole("button", { name: "Tiers" });
    expect(tiers.getAttribute("aria-current")).toBe("page");
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
    // The models table lists which ladder rungs reference each model.
    expect(screen.getByText("Trivial, Routine, High")).toBeTruthy();
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

  it("Discard changes reverts the edit instead of adopting it", async () => {
    makeTransport();
    await openDialog();
    await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: "https://edited.example/v1/" } });
    // Leaving alpha for beta raises the in-page banner; discarding must put
    // alpha's saved values back, not crown the edited draft as the baseline.
    await fireEvent.click(screen.getByRole("button", { name: "Edit beta" }));
    await fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
    expect((screen.getByLabelText("Base URL") as HTMLInputElement).value).toBe("https://beta.example/v1/");

    await fireEvent.click(screen.getByRole("button", { name: "Edit alpha" }));
    expect((screen.getByLabelText("Base URL") as HTMLInputElement).value).toBe("https://alpha.example/v1/");
    expect(screen.queryByText("Unsaved changes")).toBeNull();
  });

  it("Discard changes reverts the draft even when the queued navigation no longer resolves", async () => {
    // The page on its own: the endpoint the banner was queued for can vanish
    // before the user answers, leaving the queued navigation a no-op. A discard
    // that only marks the edited draft clean would then show it as saved, with
    // no dirty footer — the old handler's "adopt the draft" defect.
    const transport = makeTransport();
    const alpha = endpoint("alpha");
    const beta = endpoint("beta");
    const page = render(EndpointsPage, { transport, endpoints: [alpha, beta], tiers: emptyTiers(), onendpoints: vi.fn() });
    await fireEvent.input(await screen.findByLabelText("Base URL"), { target: { value: "https://edited.example/v1/" } });
    await fireEvent.click(screen.getByRole("button", { name: "Edit beta" }));
    expect(screen.getByText("Discard unsaved changes?")).toBeTruthy();

    await page.rerender({ transport, endpoints: [alpha], tiers: emptyTiers(), onendpoints: vi.fn() });
    await fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));

    expect((screen.getByLabelText("Base URL") as HTMLInputElement).value).toBe("https://alpha.example/v1/");
    expect(screen.queryByText("Unsaved changes")).toBeNull();
  });

  it("a failed endpoints read leaves Safety, MCP and Appearance usable and shows the error on Endpoints", async () => {
    makeTransport({ endpoints: vi.fn(async () => Promise.reject(new Error("settings host unavailable"))) });
    makeSafetyTransport();
    makeMcpTransport();
    const appearance: AppearanceSettings = {
      sound_complete: true,
      sound_input_needed: true,
      sound_stopped_errors: true,
      theme: "light",
      close_behavior: "auto",
    };
    setPreviewAppearanceTransport({
      get: vi.fn(async () => appearance),
      save: vi.fn(async () => appearance),
    });
    render(SettingsDialog, { open: true, onclose: vi.fn(), initialPage: "endpoints" });

    // Endpoints needs the read, so it reports the failure in place of the editor.
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("settings host unavailable");
    expect(screen.queryByLabelText("Base URL")).toBeNull();

    // The pages that read neither endpoints nor tiers keep working.
    await fireEvent.click(screen.getByRole("button", { name: "Safety" }));
    expect(await screen.findByRole("region", { name: "Approval mode" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "MCP" }));
    expect(await screen.findByRole("region", { name: "MCP servers" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Appearance" }));
    expect(await screen.findByRole("region", { name: "Sounds" })).toBeTruthy();
    setPreviewAppearanceTransport(null);
  });

  it("a failed tiers re-read after an endpoint save shows an alert, and a later save clears it", async () => {
    const transport = makeTransport();
    await openDialog();
    // The dialog's own read succeeded; the re-read after the save is what fails.
    transport.tiers.mockRejectedValueOnce(new Error("tier store locked"));

    await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: "https://edited.example/v1/" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));

    const alert = await screen.findByText(/Could not re-read tiers after the endpoint change: tier store locked/);
    expect(alert.getAttribute("role")).toBe("alert");

    // A later re-read that works takes the warning down again.
    await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: "https://edited-again.example/v1/" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByText(/Could not re-read tiers/)).toBeNull());
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
    const transport = makeTransport({ endpointUsage: vi.fn(async () => ["tier high: alpha/m1"]) });
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Delete endpoint" }));
    expect(await screen.findByText("tier high: alpha/m1")).toBeTruthy();
    expect(transport.removeEndpoint).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole("button", { name: "Confirm delete" }));
    await waitFor(() => expect(transport.removeEndpoint).toHaveBeenCalledWith("alpha"));
  });

  it("adds, reorders, removes tier models and saves", async () => {
    const transport = makeTransport();
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Tiers" }));
    const routine = await screen.findByRole("region", { name: "Routine tier" });
    await fireEvent.click(within(routine).getByRole("button", { name: "Move alpha/m2 up" }));
    await fireEvent.click(within(routine).getByRole("button", { name: "Remove alpha/m1 from Routine" }));
    const medium = screen.getByRole("region", { name: "Medium tier" });
    await fireEvent.change(within(medium).getByLabelText("Add a model to Medium"), { target: { value: "beta/m1" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save tiers" }));
    await waitFor(() =>
      expect(transport.saveTiers).toHaveBeenCalledWith({
        tiers: [
          { id: "trivial", name: "Trivial", definition: "Single-file edits, checks, lookups, scaffolding", models: ["alpha/m1"], effort: null, role: null, resolves_to: "trivial" },
          { id: "routine", name: "Routine", definition: "Typical implementation steps", models: ["alpha/m2"], effort: null, role: null, resolves_to: "trivial" },
          { id: "medium", name: "Medium", definition: "Multi-file change with a clear spec", models: ["beta/m1"], effort: null, role: null, resolves_to: "routine" },
          { id: "high", name: "High", definition: "Design, debugging, bug fixes, shared contracts", models: ["alpha/m1"], effort: null, role: null, resolves_to: "medium" },
          { id: "escalate", name: "Escalate", definition: "Only after a lower tier failed", models: [], effort: null, role: "escalate", resolves_to: "high" },
        ],
      }),
    );
  });

  it("shows tier validation errors", async () => {
    makeTransport({ saveTiers: vi.fn(async () => Promise.reject("unknown model x/y")) });
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Tiers" }));
    const medium = await screen.findByRole("region", { name: "Medium tier" });
    await fireEvent.change(within(medium).getByLabelText("Add a model to Medium"), { target: { value: "alpha/m1" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save tiers" }));
    expect(await screen.findByText("unknown model x/y")).toBeTruthy();
  });

  it("shows the Models and Savings pages beside Endpoints and Tiers", async () => {
    makeTransport();
    mocks.modelTransport = {
      get: vi.fn(async () => ({
        task_models: { summarize: null },
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
          idle_compaction: true,
          archive_min_chars: 2400,
          compress_min_chars: 3000,
          lean_output_fill_pct: 80,
          nudge_cooldown_requests: 4,
          compress_extra_commands: [],
        },
        budget: { enabled: false, max_tokens_per_plan: 0, max_escalations_per_plan: 0, max_retries_per_lane: 0 },
      })),
      save: vi.fn(async () => undefined),
      install: vi.fn(async () => undefined),
      onEmbedderEvent: vi.fn(async () => () => {}),
    };
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Models" }));
    const work = await screen.findByRole("region", { name: "Compaction" });
    expect(within(work).getByLabelText("Compaction model")).toBeTruthy();
    expect(screen.getByText(/Mnemo1's per-task models/)).toBeTruthy();
    expect(screen.getByText("Status: Keyword only")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Savings" }));
    const levers = await screen.findByRole("region", { name: "Savings levers" });
    expect(within(levers).getByText("Outline reads")).toBeTruthy();
    expect(screen.getByText("Model prices are set with each model on the Endpoints page.")).toBeTruthy();
  });

  it("opens the Tiers page's other Laya features and shows the readiness line", async () => {
    makeTransport();
    makeLayaTransport();
    await openDialog();
    // The retired Decisions tab is gone; the Laya content rides Tiers.
    expect(screen.queryByRole("button", { name: "Decisions" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Tiers" }));
    const other = await screen.findByText("Other things Laya can do");
    await fireEvent.click(other);
    expect((screen.getByRole("checkbox", { name: /Auto-type memories/ }) as HTMLInputElement).checked).toBe(true);
    // The top control reads its own wire: the checkpoint and the kNN count.
    expect(await screen.findByText(/Learned from 42 finished workers/)).toBeTruthy();
    expect(screen.getByText(/Laya \(English\) ready/)).toBeTruthy();
  });

  it("passes the store's laya pass to the page", async () => {
    makeTransport();
    makeLayaTransport();
    app.set({
      ...fixtureState(),
      indexing: {
        revision: 3,
        firstRun: false,
        code: null,
        memory: null,
        laya: { state: "running", done: 34, total: 207, detail: null },
      },
    });
    render(SettingsDialog, { open: true, onclose: vi.fn(), initialPage: "tiers" });
    expect(await screen.findByText(/Checking Laya/)).toBeTruthy();
  });

  it("opens the Tiers page when asked for the retired Decisions page", async () => {
    makeTransport();
    render(SettingsDialog, { open: true, onclose: vi.fn(), initialPage: "laya" });
    const tiers = await screen.findByRole("button", { name: "Tiers" });
    expect(tiers.getAttribute("aria-current")).toBe("page");
    expect(screen.queryByRole("button", { name: "Decisions" })).toBeNull();
  });

  it("explains Laya settings need the desktop app without a Laya transport", async () => {
    makeTransport();
    await openDialog();
    await fireEvent.click(screen.getByRole("button", { name: "Tiers" }));
    expect(await screen.findByText("Laya settings are available in the desktop app.")).toBeTruthy();
  });

  it("the About page shows the version and opens links", async () => {
    makeTransport();
    const aboutTransport = { version: async () => "9.9.9", open: vi.fn(async () => {}) };
    mocks.aboutTransport = aboutTransport;
    await openDialog();

    await fireEvent.click(screen.getByRole("button", { name: "About" }));
    expect(await screen.findByText("v9.9.9")).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Source code" }));
    expect(aboutTransport.open).toHaveBeenCalledWith("https://github.com/chessIthaca/Mnemo");
  });
});

/**
 * The dialog-level unsaved guard: every draft page reports through one
 * registry, and closing or switching pages asks first.
 */
describe("unsaved edits", () => {
  beforeEach(() => {
    mocks.transport = null;
    mocks.modelTransport = null;
    mocks.layaTransport = null;
    mocks.safetyTransport = null;
    mocks.mcpTransport = null;
    mocks.specsTransport = null;
    mocks.aboutTransport = null;
    app.set(fixtureState());
  });

  /** The Models and Savings pages share the model-settings transport. */
  function makeModelTransport() {
    mocks.modelTransport = {
      get: vi.fn(async () => ({
        task_models: { summarize: null },
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
          idle_compaction: true,
          archive_min_chars: 2400,
          compress_min_chars: 3000,
          lean_output_fill_pct: 80,
          nudge_cooldown_requests: 4,
          compress_extra_commands: [],
        },
        budget: { enabled: false, max_tokens_per_plan: 0, max_escalations_per_plan: 0, max_retries_per_lane: 0 },
      })),
      save: vi.fn(async () => undefined),
      install: vi.fn(async () => undefined),
      onEmbedderEvent: vi.fn(async () => () => {}),
    };
  }

  function makeSpecsTransport() {
    const schemas = {
      schemas: [
        {
          name: "default",
          reference: "default",
          description: "",
          origin: "project",
          path: ".coding/openspec/schemas/default",
          artifacts: ["context"],
          warnings: [],
          error: null,
        },
      ],
      active: "default",
      active_error: null,
    };
    const artifacts = { name: "default", reference: "default", origin: "project", description: "", artifacts: [] };
    const config = { schema: "default", context: "Original context", rules: {}, warnings: [] };
    mocks.specsTransport = {
      status: vi.fn(async () => ({ capabilities: [], pending_plans: [], openspec_cli: true })),
      listSchemas: vi.fn(async () => schemas),
      setActiveSchema: vi.fn(async () => schemas),
      addSchema: vi.fn(async () => schemas),
      forkSchema: vi.fn(async () => schemas),
      readArtifacts: vi.fn(async () => artifacts),
      saveArtifact: vi.fn(async () => artifacts),
      readConfig: vi.fn(async () => config),
      saveConfig: vi.fn(async () => config),
      planPreview: vi.fn(async () => ({ name: "", reference: "", description: "", sections: [], required: [], worker_context: "" })),
      runValidate: vi.fn(async () => ({ exit_code: 0, stdout: "", stderr: "" })),
    };
  }

  /** Open the dialog on a fresh instance and hand back the close spy. */
  function openSettings(initialPage?: "project" | "specs" | "endpoints" | "tiers" | "models" | "savings" | "safety" | "mcp" | "appearance") {
    makeTransport();
    const onclose = vi.fn();
    render(SettingsDialog, { open: true, onclose, ...(initialPage ? { initialPage } : {}) });
    return onclose;
  }

  /** One draft page: how to reach it, one unsaved edit, and proof it survived. */
  type PageCase = {
    name: string;
    prepare: () => void;
    open: () => Promise<void>;
    edit: () => Promise<void>;
    kept: () => boolean;
  };

  const editedUrl = "https://edited.example/v1/";
  const editedDefinition = "Edited: cheap chatter only";

  const pages: PageCase[] = [
    {
      name: "Endpoints",
      prepare: () => {},
      open: async () => {
        await screen.findByRole("button", { name: "Edit alpha" });
      },
      edit: async () => {
        await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: editedUrl } });
      },
      kept: () => (screen.getByLabelText("Base URL") as HTMLInputElement).value === editedUrl,
    },
    {
      name: "Tiers",
      prepare: () => {},
      open: async () => {
        await screen.findByRole("button", { name: "Edit alpha" });
        await fireEvent.click(screen.getByRole("button", { name: "Tiers" }));
        await screen.findByRole("region", { name: "Routine tier" });
      },
      edit: async () => {
        const routine = screen.getByRole("region", { name: "Routine tier" });
        await fireEvent.input(within(routine).getByLabelText(/^Definition/), { target: { value: editedDefinition } });
      },
      kept: () =>
        (within(screen.getByRole("region", { name: "Routine tier" })).getByLabelText(/^Definition/) as HTMLTextAreaElement)
          .value === editedDefinition,
    },
    {
      name: "Models",
      prepare: makeModelTransport,
      open: async () => {
        await screen.findByRole("button", { name: "Edit alpha" });
        await fireEvent.click(screen.getByRole("button", { name: "Models" }));
        await screen.findByLabelText("Compaction model");
      },
      edit: async () => {
        await fireEvent.input(screen.getByLabelText("Compaction model"), { target: { value: "alpha/m1" } });
      },
      kept: () => (screen.getByLabelText("Compaction model") as HTMLInputElement).value === "alpha/m1",
    },
    {
      name: "Savings",
      prepare: makeModelTransport,
      open: async () => {
        await screen.findByRole("button", { name: "Edit alpha" });
        await fireEvent.click(screen.getByRole("button", { name: "Savings" }));
        await screen.findByRole("region", { name: "Savings levers" });
      },
      edit: async () => {
        await fireEvent.click(screen.getByRole("checkbox", { name: /Outline reads/ }));
      },
      kept: () => !(screen.getByRole("checkbox", { name: /Outline reads/ }) as HTMLInputElement).checked,
    },
    {
      name: "Safety",
      prepare: makeSafetyTransport,
      open: async () => {
        await screen.findByRole("button", { name: "Edit alpha" });
        await fireEvent.click(screen.getByRole("button", { name: "Safety" }));
        await screen.findByRole("region", { name: "Approval mode" });
      },
      edit: async () => {
        await fireEvent.click(screen.getByRole("radio", { name: /Run without asking/ }));
      },
      kept: () => (screen.getByRole("radio", { name: /Run without asking/ }) as HTMLInputElement).checked,
    },
    {
      name: "MCP",
      prepare: makeMcpTransport,
      open: async () => {
        await screen.findByRole("button", { name: "Edit alpha" });
        await fireEvent.click(screen.getByRole("button", { name: "MCP" }));
        await screen.findByRole("region", { name: "MCP servers" });
      },
      edit: async () => {
        await fireEvent.click(screen.getByRole("checkbox", { name: "Enabled" }));
      },
      kept: () => !(screen.getByRole("checkbox", { name: "Enabled" }) as HTMLInputElement).checked,
    },
    {
      name: "Specs",
      prepare: makeSpecsTransport,
      open: async () => {
        await screen.findByRole("button", { name: "Edit alpha" });
        await fireEvent.click(screen.getByRole("button", { name: "Specs" }));
        const context = await screen.findByLabelText("Project context");
        await waitFor(() => expect((context as HTMLTextAreaElement).disabled).toBe(false));
      },
      edit: async () => {
        await fireEvent.input(screen.getByLabelText("Project context"), { target: { value: "Edited context" } });
      },
      kept: () => (screen.getByLabelText("Project context") as HTMLTextAreaElement).value === "Edited context",
    },
  ];

  for (const entry of pages) {
    it(`${entry.name}: X with an unsaved edit asks, Keep editing keeps it, Discard changes closes`, async () => {
      entry.prepare();
      const onclose = openSettings();
      await entry.open();
      await entry.edit();

      await fireEvent.click(screen.getByRole("button", { name: "Close" }));
      const banner = await screen.findByRole("alert");
      expect(within(banner).getByText("Discard unsaved changes?")).toBeTruthy();
      expect(onclose).not.toHaveBeenCalled();

      await fireEvent.click(within(banner).getByRole("button", { name: "Keep editing" }));
      expect(screen.queryByText("Discard unsaved changes?")).toBeNull();
      expect(onclose).not.toHaveBeenCalled();
      expect(entry.kept()).toBe(true);

      await fireEvent.click(screen.getByRole("button", { name: "Close" }));
      const again = await screen.findByRole("alert");
      await fireEvent.click(within(again).getByRole("button", { name: "Discard changes" }));
      expect(onclose).toHaveBeenCalledTimes(1);
    });
  }


  it("Escape asks instead of closing", async () => {
    const onclose = openSettings();
    await screen.findByRole("button", { name: "Edit alpha" });
    await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: editedUrl } });

    await fireEvent.keyDown(window, { key: "Escape" });
    expect(await screen.findByText("Discard unsaved changes?")).toBeTruthy();
    expect(onclose).not.toHaveBeenCalled();
  });

  it("the backdrop asks instead of closing", async () => {
    const onclose = openSettings();
    await screen.findByRole("button", { name: "Edit alpha" });
    await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: editedUrl } });

    await fireEvent.click(screen.getByRole("button", { name: "Close sheet" }));
    expect(await screen.findByText("Discard unsaved changes?")).toBeTruthy();
    expect(onclose).not.toHaveBeenCalled();
  });

  it("a rail click asks and switches only after Discard changes", async () => {
    const onclose = openSettings();
    await screen.findByRole("button", { name: "Edit alpha" });
    await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: editedUrl } });

    await fireEvent.click(screen.getByRole("button", { name: "Tiers" }));
    expect(await screen.findByText("Discard unsaved changes?")).toBeTruthy();
    // Still on Endpoints: the rail did not move and the draft is intact.
    expect(screen.getByRole("button", { name: "Endpoints" }).getAttribute("aria-current")).toBe("page");
    expect((screen.getByLabelText("Base URL") as HTMLInputElement).value).toBe(editedUrl);

    await fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Tiers" }).getAttribute("aria-current")).toBe("page"));
    expect(onclose).not.toHaveBeenCalled();
  });

  it("Escape while asking keeps the question up", async () => {
    const onclose = openSettings();
    await screen.findByRole("button", { name: "Edit alpha" });
    await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: editedUrl } });

    await fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(await screen.findByText("Discard unsaved changes?")).toBeTruthy();

    // Escape and a second X both leave the one question standing.
    await fireEvent.keyDown(window, { key: "Escape" });
    await fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.getByText("Discard unsaved changes?")).toBeTruthy();
    expect(onclose).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));
    expect(screen.queryByText("Discard unsaved changes?")).toBeNull();
    expect((screen.getByLabelText("Base URL") as HTMLInputElement).value).toBe(editedUrl);
  });

  it("focus lands on Keep editing", async () => {
    openSettings();
    await screen.findByRole("button", { name: "Edit alpha" });
    await fireEvent.input(screen.getByLabelText("Base URL"), { target: { value: editedUrl } });

    await fireEvent.click(screen.getByRole("button", { name: "Close" }));
    const keep = await screen.findByRole("button", { name: "Keep editing" });
    await waitFor(() => expect(document.activeElement).toBe(keep));
  });

  it("a clean page closes at once with no question", async () => {
    const onclose = openSettings();
    await screen.findByRole("button", { name: "Edit alpha" });

    await fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onclose).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("Discard unsaved changes?")).toBeNull();
  });

  it("a clean page switches at once", async () => {
    const onclose = openSettings();
    await screen.findByRole("button", { name: "Edit alpha" });

    await fireEvent.click(screen.getByRole("button", { name: "Tiers" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Tiers" }).getAttribute("aria-current")).toBe("page"));
    expect(screen.queryByText("Discard unsaved changes?")).toBeNull();
    expect(onclose).not.toHaveBeenCalled();
  });

  it("opening Tiers with no edits does not ask before leaving", async () => {
    // The routing wire has no `effort` key (see makeLayaTransport), exactly
    // like the real backend; the old page seeded its draft with one and then
    // asked about changes that were never made. This is the regression test
    // for that false save prompt.
    makeLayaTransport();
    const onclose = openSettings("tiers");
    await screen.findByText(/Laya \(English\) ready/);
    await fireEvent.click(screen.getByRole("button", { name: "Endpoints" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Endpoints" }).getAttribute("aria-current")).toBe("page"));
    expect(screen.queryByText("Discard unsaved changes?")).toBeNull();
    expect(onclose).not.toHaveBeenCalled();
  });

  it("Project and Appearance never ask", async () => {
    // Project applies on change, so there is nothing to protect.
    const onclose = openSettings("project");
    const slots = await screen.findByLabelText("Project worker slots");
    await fireEvent.change(slots, { target: { value: "5" } });
    await fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onclose).toHaveBeenCalledTimes(1);
    cleanup();

    // Appearance saves each toggle as it is clicked.
    const appearance: AppearanceSettings = {
      sound_complete: true,
      sound_input_needed: true,
      sound_stopped_errors: true,
      theme: "light",
      close_behavior: "auto",
    };
    setPreviewAppearanceTransport({
      get: vi.fn(async () => appearance),
      save: vi.fn(async () => ({ ...appearance, sound_complete: false })),
    });

    const again = openSettings("appearance");
    const sounds = await screen.findByRole("region", { name: "Sounds" });
    expect(within(sounds).getAllByRole("checkbox").length).toBeGreaterThan(0);
    await fireEvent.click(within(sounds).getAllByRole("checkbox")[0]);
    await fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(again).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("Discard unsaved changes?")).toBeNull();
    setPreviewAppearanceTransport(null);
  });

  it("an open MCP form counts as unsaved", async () => {
    makeMcpTransport();
    const onclose = openSettings("mcp");
    await screen.findByRole("region", { name: "MCP servers" });

    // Typing into the add form leaves the saved list untouched but holds values.
    await fireEvent.click(screen.getByRole("button", { name: "Add server" }));
    const editor = await screen.findByRole("region", { name: "Server editor" });
    await fireEvent.input(within(editor).getByLabelText("Name"), { target: { value: "half-typed" } });

    await fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(await screen.findByText("Discard unsaved changes?")).toBeTruthy();
    expect(onclose).not.toHaveBeenCalled();
  });
});
