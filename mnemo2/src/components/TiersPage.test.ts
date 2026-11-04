import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { emptyModel, type EndpointDetailDto, type SettingsTransport, type TierDto, type TiersDto } from "../lib/settingsEngine";
import type { LayaStatus, LayaTransport, RoutingStatusWire, TiersTopControl } from "../lib/layaSettings";
import TiersPage from "./TiersPage.svelte";

/**
 * The Settings > Tiers page (design §7) against in-memory transports: the
 * top control's readiness states and one-save picks, the ladder editor's
 * required-trivial and fold hints, the Laya labels, and the routing details
 * that only render while Laya helps.
 */

function endpoint(name: string, models: string[]): EndpointDetailDto {
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
    models: models.map((id) => ({
      ...emptyModel(id),
      price: { input_per_1m: 1.5, output_per_1m: 2.5, cached_per_1m: 0 },
    })),
  };
}

/** One rung; per-test overrides keep the fixtures short. */
function rung(id: string, overrides: Partial<TierDto> = {}): TierDto {
  return {
    id,
    name: id[0].toUpperCase() + id.slice(1),
    definition: `${id} work`,
    models: ["alpha/m1"],
    effort: null,
    role: id === "escalate" ? "escalate" : null,
    resolves_to: { trivial: "trivial", routine: "trivial", medium: "routine", high: "medium", escalate: "high" }[id] ?? "trivial",
    ...overrides,
  };
}

/** A five-rung ladder; empty tiers fold down per test. */
function ladder(overrides: Record<string, Partial<TierDto>> = {}): TiersDto {
  return {
    tiers: [
      rung("trivial", overrides.trivial),
      rung("routine", overrides.routine),
      rung("medium", overrides.medium),
      rung("high", overrides.high),
      rung("escalate", overrides.escalate),
    ],
  };
}

/** The status the page opens with, with per-test overrides. */
function status(overrides: Partial<LayaStatus> = {}): LayaStatus {
  return {
    enabled: true,
    installed: true,
    status: "ready",
    flags: {
      auto_type_memories: false,
      failure_triage: false,
      failure_triage_knn: false,
      auto_finetune: false,
      steer_tool_choice: false,
      routing: true,
      reflex: false,
    },
    routing: { threshold: 0.8, first_prompt_min_confidence: null, enforce: false },
    ...overrides,
  };
}

/** The top control's wire, with per-test overrides. */
function control(overrides: Partial<TiersTopControl> = {}): TiersTopControl {
  return {
    choice: "suggest",
    mnemo1_enforce: false,
    laya_routing: true,
    laya_status: "ready",
    checkpoint: "Laya (English)",
    knn_rows: 42,
    knn_min_rows: 30,
    ...overrides,
  };
}

/** The routing-details wire, with per-test overrides. */
function routingWireSnapshot(overrides: Partial<RoutingStatusWire["settings"]> = {}): RoutingStatusWire {
  return {
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
      effort: {},
      ...overrides,
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
  };
}

/** An in-memory Laya transport; every method is a spy the tests can read. */
function makeLaya(overrides: { control?: Partial<TiersTopControl>; layaStatus?: Partial<LayaStatus> } = {}) {
  let nextControl = control(overrides.control);
  let nextStatus = status(overrides.layaStatus);
  const transport: LayaTransport = {
    status: vi.fn(async () => structuredClone(nextStatus)),
    topControl: vi.fn(async () => structuredClone(nextControl)),
    routingStatus: vi.fn(async () => routingWireSnapshot({ mode: nextControl.choice })),
    saveTopControl: vi.fn(async (choice) => {
      const enforce = choice === "auto";
      const layaRouting = choice !== "off";
      nextControl = {
        ...nextControl,
        choice,
        mnemo1_enforce: enforce,
        laya_routing: layaRouting,
      };
      return structuredClone(nextControl);
    }),
    setup: vi.fn(async () => undefined),
    disable: vi.fn(async () => undefined),
    save: vi.fn(async () => undefined),
    saveRoutingSettings: vi.fn(async (settings) => routingWireSnapshot(settings)),
    runRoutingEval: vi.fn(),
    onLayaEvent: vi.fn(async () => () => {}),
  };
  return { transport, readControl: () => nextControl };
}

/** An in-memory settings transport; only the ladder rides it here. */
function makeSettings(loaded: TiersDto) {
  return {
    endpoints: vi.fn(async () => [endpoint("alpha", ["m1"])]),
    upsertEndpoint: vi.fn(),
    removeEndpoint: vi.fn(),
    setApiKey: vi.fn(),
    testEndpoint: vi.fn(),
    listProviderModels: vi.fn(),
    endpointUsage: vi.fn(async () => []),
    tiers: vi.fn(async () => loaded),
    saveTiers: vi.fn(async (next: TiersDto) => structuredClone(next)),
  } as unknown as SettingsTransport;
}

function renderPage(options: {
  laya?: ReturnType<typeof makeLaya>;
  ladder?: TiersDto;
} = {}) {
  const laya = options.laya ?? makeLaya();
  const loaded = options.ladder ?? ladder({ medium: { models: [] }, escalate: { models: [] } });
  const settings = makeSettings(loaded);
  const rendered = render(TiersPage, {
    transport: settings,
    layaTransport: laya.transport,
    endpoints: [endpoint("alpha", ["m1"])],
    tiers: loaded,
    ontiers: () => {},
  });
  return { ...rendered, laya, settings, loaded };
}

describe("TiersPage", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("disables the three Laya choices when Laya is not installed and offers the install", async () => {
    renderPage({ laya: makeLaya({ control: { choice: "off", laya_status: "not_installed", laya_routing: false } }) });
    // The readiness line only renders once the control's wire has loaded.
    await screen.findByText("Laya is not installed");
    const watch = screen.getByRole("radio", { name: /Watch only/ });
    const suggest = screen.getByRole("radio", { name: /^Suggest/ });
    const auto = screen.getByRole("radio", { name: /^Auto/ });
    expect((watch as HTMLInputElement).disabled).toBe(true);
    expect((suggest as HTMLInputElement).disabled).toBe(true);
    expect((auto as HTMLInputElement).disabled).toBe(true);
    // The lead-picks choice stays pickable.
    expect((screen.getByRole("radio", { name: /No: the lead picks/ }) as HTMLInputElement).disabled).toBe(false);
    expect(screen.getByRole("button", { name: "Install Laya" })).toBeTruthy();
  });

  it("offers Turn on while Laya is off, and a warning that the saved choice cannot run", async () => {
    renderPage({ laya: makeLaya({ control: { choice: "shadow", laya_status: "disabled" } }) });
    await screen.findByRole("button", { name: "Turn on" });
    expect(screen.getByText("Laya is off")).toBeTruthy();
    expect(screen.getByText(/needs Laya running/)).toBeTruthy();
  });

  it("offers Retry with the failure's error after a failed start", async () => {
    renderPage({ laya: makeLaya({ control: { choice: "suggest", laya_status: "failed" } }) });
    await screen.findByRole("button", { name: "Retry" });
    expect(screen.getByText("Laya failed to start")).toBeTruthy();
  });

  it("shows the checkpoint and the kNN labelled-worker count when ready", async () => {
    renderPage({ laya: makeLaya({ control: { choice: "suggest", laya_status: "ready", checkpoint: "Laya (English)", knn_rows: 42, knn_min_rows: 30 } }) });
    expect(await screen.findByText("Laya (English) ready")).toBeTruthy();
    expect(screen.getByText("kNN: 42 labelled workers (answers from 30)")).toBeTruthy();
  });

  it("saves the picked choice once through the combined command", async () => {
    const page = renderPage({ laya: makeLaya({ control: { choice: "off", laya_routing: false } }) });
    await screen.findByRole("radio", { name: /Watch only/ });
    await fireEvent.click(screen.getByRole("radio", { name: /Watch only/ }));
    await waitFor(() => expect(page.laya.transport.saveTopControl).toHaveBeenCalledTimes(1));
    expect(page.laya.transport.saveTopControl).toHaveBeenCalledWith("shadow");
  });

  it("keeps the trivial tier's last model — the remove button refuses", async () => {
    renderPage({ laya: makeLaya() });
    const trivial = await screen.findByRole("region", { name: "Trivial tier" });
    expect(
      (within(trivial).getByRole("button", { name: "Remove alpha/m1 from Trivial" }) as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(within(trivial).getByText("required: the default model")).toBeTruthy();
  });

  it("refuses to save a ladder whose trivial tier has no model", async () => {
    // A hand-edited endpoints.toml can load trivial empty; the page refuses
    // to save it back, naming the required default model.
    const page = renderPage({ ladder: ladder({ trivial: { models: [] } }) });
    await screen.findByRole("button", { name: "Save tiers" });
    await fireEvent.click(screen.getByRole("button", { name: "Save tiers" }));
    expect(
      await screen.findByText(/The trivial tier must have a model/),
    ).toBeTruthy();
    expect(page.settings.saveTiers).not.toHaveBeenCalled();
  });

  it("hints at the tier an empty tier folds down to, and notes escalate", async () => {
    renderPage({ laya: makeLaya() });
    const medium = await screen.findByRole("region", { name: "Medium tier" });
    expect(within(medium).getByText("uses Routine's model")).toBeTruthy();
    const escalate = screen.getByRole("region", { name: "Escalate tier" });
    expect(within(escalate).getByText("only after a lower tier failed")).toBeTruthy();
  });

  it("shows the Laya label chips only while Laya helps pick", async () => {
    const helping = renderPage({ laya: makeLaya({ control: { choice: "suggest" } }) });
    const helpingScope = within(helping.container);
    // The kNN line only renders once the control's wire has loaded.
    await waitFor(() => expect(helpingScope.getByText("kNN: 42 labelled workers (answers from 30)")).toBeTruthy());
    expect(helpingScope.getByText("Laya: involved")).toBeTruthy();
    expect(helpingScope.getByText("Laya: hard")).toBeTruthy();
    expect(helpingScope.getByText("Laya: routine")).toBeTruthy();
    // Escalate is never offered a label.
    const escalate = helpingScope.getByRole("region", { name: "Escalate tier" });
    expect(within(escalate).queryByText(/Laya:/)).toBeNull();

    const notHelping = renderPage({ laya: makeLaya({ control: { choice: "off", laya_routing: false } }) });
    const offScope = within(notHelping.container);
    await waitFor(() => expect(offScope.getByRole("region", { name: "Medium tier" })).toBeTruthy());
    // With the lead picking, no rung carries a Laya label.
    expect(offScope.queryByText("Laya: involved")).toBeNull();
    expect(offScope.queryByText("Laya: routine")).toBeNull();
  });

  it("hides the routing details for 'No: the lead picks' and shows them when Laya helps", async () => {
    const off = renderPage({ laya: makeLaya({ control: { choice: "off", laya_routing: false } }) });
    await screen.findByRole("region", { name: "Tier ladder" });
    expect(off.container.textContent).not.toContain("Routing details");

    const helping = renderPage({ laya: makeLaya({ control: { choice: "suggest" } }) });
    const summary = await screen.findByText("Routing details");
    await fireEvent.click(summary);
    expect(await screen.findByLabelText("Four-label gate")).toBeTruthy();
    expect((screen.getByLabelText("Four-label gate") as HTMLInputElement).value).toBe("0.4");
  });
});
