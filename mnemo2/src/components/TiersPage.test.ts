import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { IndexPass } from "../lib/indexingEngine";
import { emptyModel, type EndpointDetailDto, type SettingsTransport, type TierDto, type TiersDto } from "../lib/settingsEngine";
import type { EvalRecord, LayaFlagKey, LayaStatus, LayaTransport, RoutingStatusWire, TiersTopControl } from "../lib/layaSettings";
import TiersPage from "./TiersPage.svelte";

/**
 * The Settings > Tiers page (plan 8106337a) against in-memory transports: the
 * top control's readiness states and one-save picks (with a hint, not a lock,
 * below the unlock count), the self-running Laya check with its progress,
 * result and failure states, the ladder editor's required-trivial and fold
 * hints, and the read-only collapsed sections. No tuning inputs are left:
 * the check applies the gates itself.
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

/** The routing wire, with per-test overrides (no `effort` key — the real
 * backend never sends one, and the old draft seeding it was the false
 * save prompt). */
function routingWireSnapshot(
  settingsOverrides: Partial<RoutingStatusWire["settings"]> = {},
  summaryOverrides: Partial<RoutingStatusWire["summary"]> = {},
  evalRecord: EvalRecord | null = null,
): RoutingStatusWire {
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
      ...settingsOverrides,
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
      ...summaryOverrides,
    },
    eval: evalRecord,
  };
}

/** The last check's record, with per-test overrides. */
function evalRecord(overrides: Partial<EvalRecord> = {}): EvalRecord {
  return {
    date: "2026-11-05",
    min_confidence_4: 0.5,
    min_confidence_3: 0.475,
    min_confidence_2: 0.6,
    routed_share: 0.017,
    accuracy: 1,
    fingerprint: null,
    ...overrides,
  };
}

/** An in-memory Laya transport; every method is a spy the tests can read. */
function makeLaya(
  overrides: {
    control?: Partial<TiersTopControl>;
    layaStatus?: Partial<LayaStatus>;
    routing?: {
      settings?: Partial<RoutingStatusWire["settings"]>;
      summary?: Partial<RoutingStatusWire["summary"]>;
      eval?: EvalRecord | null;
    };
    saveFlagRejects?: boolean;
  } = {},
) {
  let nextControl = control(overrides.control);
  let nextStatus = status(overrides.layaStatus);
  const routing = overrides.routing ?? {};
  const transport: LayaTransport = {
    status: vi.fn(async () => structuredClone(nextStatus)),
    topControl: vi.fn(async () => structuredClone(nextControl)),
    routingStatus: vi.fn(async () =>
      routingWireSnapshot({ mode: nextControl.choice, ...routing.settings }, routing.summary, routing.eval ?? null),
    ),
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
    saveFlag: vi.fn(async (key: LayaFlagKey, value: boolean) => {
      if (overrides.saveFlagRejects) throw new Error("Laya settings unavailable");
      nextStatus = { ...nextStatus, flags: { ...nextStatus.flags, [key]: value } };
    }),
    checkLaya: vi.fn(async () => undefined),
    onLayaEvent: vi.fn(async () => () => {}),
  };
  return { transport, readControl: () => nextControl };
}

/** An in-memory settings transport; only the ladder rides it here. */
function makeSettings(loaded: TiersDto, saveTiers?: (next: TiersDto) => Promise<TiersDto>) {
  return {
    endpoints: vi.fn(async () => [endpoint("alpha", ["m1"])]),
    upsertEndpoint: vi.fn(),
    removeEndpoint: vi.fn(),
    setApiKey: vi.fn(),
    testEndpoint: vi.fn(),
    listProviderModels: vi.fn(),
    endpointUsage: vi.fn(async () => []),
    tiers: vi.fn(async () => loaded),
    saveTiers: saveTiers ?? vi.fn(async (next: TiersDto) => structuredClone(next)),
  } as unknown as SettingsTransport;
}

function renderPage(
  options: {
    laya?: ReturnType<typeof makeLaya>;
    ladder?: TiersDto;
    evalPass?: IndexPass | null;
    saveTiers?: (next: TiersDto) => Promise<TiersDto>;
    ontiers?: (next: TiersDto) => void;
  } = {},
) {
  const laya = options.laya ?? makeLaya();
  const loaded = options.ladder ?? ladder({ medium: { models: [] }, escalate: { models: [] } });
  const settings = makeSettings(loaded, options.saveTiers);
  const props = {
    transport: settings,
    layaTransport: laya.transport,
    endpoints: [endpoint("alpha", ["m1"])],
    tiers: loaded,
    ontiers: options.ontiers ?? (() => {}),
    evalPass: options.evalPass ?? null,
  };
  const rendered = render(TiersPage, props);
  return { ...rendered, laya, settings, loaded, props };
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
    expect(screen.getByText("Learned from 42 finished workers.")).toBeTruthy();
  });

  it("says when Laya starts using what it has learned, while it is below that count", async () => {
    renderPage({ laya: makeLaya({ control: { choice: "suggest", laya_status: "ready", knn_rows: 12, knn_min_rows: 30 } }) });
    expect(await screen.findByText("Learned from 12 finished workers; it starts using them at 30.")).toBeTruthy();
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
    await waitFor(() => expect(helpingScope.getByText("Learned from 42 finished workers.")).toBeTruthy());
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

  it("hides 'How Laya is doing' for 'No: the lead picks' and shows it read-only when Laya helps", async () => {
    const off = renderPage({ laya: makeLaya({ control: { choice: "off", laya_routing: false } }) });
    await screen.findByText("Laya (English) ready");
    expect(off.container.textContent).not.toContain("How Laya is doing");

    const helping = renderPage({ laya: makeLaya() });
    const summary = await within(helping.container).findByText("How Laya is doing");
    await fireEvent.click(summary);
    expect(within(helping.container).getByText("Set by the Laya check.")).toBeTruthy();
    // The gates row is read-only text, three two-decimal numbers.
    expect(helping.container.textContent).toContain("0.40 / 0.47 / 0.60");
    expect(within(helping.container).queryByLabelText(/gate/i)).toBeNull();
  });

  it("has no tuning inputs", async () => {
    renderPage();
    await screen.findByText("Laya (English) ready");
    expect(screen.queryByLabelText("Four-label gate")).toBeNull();
    expect(screen.queryByText("Run eval")).toBeNull();
    expect(screen.queryByText(/Force on/)).toBeNull();
    expect(screen.queryByLabelText("trivial effort")).toBeNull();
  });

  it("shows the check's progress", async () => {
    renderPage({ evalPass: { state: "running", done: 34, total: 207, detail: null } });
    expect(await screen.findByText(/34 of 207 test tasks/)).toBeTruthy();
    const bar = screen.getByRole("progressbar", { name: "Laya check progress" });
    expect(bar.getAttribute("aria-valuenow")).toBe("16");
  });

  it("shows a failed check as an alert", async () => {
    renderPage({ evalPass: { state: "failed", done: 10, total: 207, detail: "Laya stopped answering" } });
    expect(
      await screen.findByText("The Laya check failed: Laya stopped answering"),
    ).toBeTruthy();
  });

  it("shows the last check and the coverage note", async () => {
    renderPage({ laya: makeLaya({ routing: { eval: evalRecord() } }) });
    expect(
      await screen.findByText("Last checked 2026-11-05: confident on 2% of test tasks, 100% of those right."),
    ).toBeTruthy();
    expect(
      screen.getByText("Laya leaves most choices to the lead until it has learned from more finished workers."),
    ).toBeTruthy();
  });

  it("Check again calls the transport, and it is disabled while running", async () => {
    const page = renderPage();
    const again = await screen.findByRole("button", { name: "Check again" });
    expect((again as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.click(again);
    await waitFor(() => expect(page.laya.transport.checkLaya).toHaveBeenCalledTimes(1));

    const busy = renderPage({ evalPass: { state: "running", done: 34, total: 207, detail: null } });
    const busyAgain = await within(busy.container).findByRole("button", { name: "Check again" });
    expect((busyAgain as HTMLButtonElement).disabled).toBe(true);
  });

  it("re-reads routing when a check finishes", async () => {
    const page = renderPage({ evalPass: { state: "running", done: 10, total: 207, detail: null } });
    await waitFor(() => expect(page.laya.transport.routingStatus).toHaveBeenCalledTimes(1));
    await page.rerender({ evalPass: { state: "done", done: 207, total: 207, detail: "Confident on 2% of test tasks" } });
    await waitFor(() => expect(page.laya.transport.routingStatus).toHaveBeenCalledTimes(2));
  });

  it("a choice below its unlock count saves directly with a hint", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const page = renderPage({ laya: makeLaya({ routing: { summary: { labelled: 0 } } }) });
    const hint = await screen.findByText(/Best after/);
    expect(hint.textContent).toContain("0 of 30 decisions with outcome labels");
    await fireEvent.click(screen.getByRole("radio", { name: /^Auto/ }));
    await waitFor(() => expect(page.laya.transport.saveTopControl).toHaveBeenCalledWith("auto"));
    // No force, no confirm — the pick saves straight through.
    expect(confirm).not.toHaveBeenCalled();
    confirm.mockRestore();
  });

  it("a flag switch saves at once and reverts on failure", async () => {
    const page = renderPage({ laya: makeLaya() });
    const box = await within(page.container).findByRole("checkbox", { name: /Reflex \(plan-step tiers, Mnemo1\)/ });
    expect((box as HTMLInputElement).checked).toBe(false);
    await fireEvent.click(box);
    await waitFor(() => expect(page.laya.transport.saveFlag).toHaveBeenCalledWith("reflex", true));
    // The save refreshed the snapshot: the switch stays on.
    expect(
      (within(page.container).getByRole("checkbox", { name: /Reflex \(plan-step tiers, Mnemo1\)/ }) as HTMLInputElement)
        .checked,
    ).toBe(true);

    const failing = renderPage({ laya: makeLaya({ saveFlagRejects: true }) });
    const failingBox = await within(failing.container).findByRole("checkbox", { name: /Reflex \(plan-step tiers, Mnemo1\)/ });
    await fireEvent.click(failingBox);
    await waitFor(() => expect(failing.laya.transport.saveFlag).toHaveBeenCalledTimes(1));
    // The save refused: the box goes back and the error shows.
    expect(
      (within(failing.container).getByRole("checkbox", { name: /Reflex \(plan-step tiers, Mnemo1\)/ }) as HTMLInputElement)
        .checked,
    ).toBe(false);
    expect(await within(failing.container).findByText("Laya settings unavailable")).toBeTruthy();
  });

  it("the ladder is not dirty after a save that changes resolves_to", async () => {
    // The backend normalizes a rung's fold target on save; the stored copy
    // then differs from the draft. The page must re-seed the draft from it,
    // or every leave asks about "unsaved" changes that were already saved.
    const saveTiers = vi.fn(async (next: TiersDto) => {
      const stored = structuredClone(next);
      stored.tiers[2].resolves_to = "trivial";
      return stored;
    });
    const ontiers = vi.fn();
    const page = renderPage({ saveTiers, ontiers });
    const routine = await screen.findByRole("region", { name: "Routine tier" });
    await fireEvent.input(within(routine).getByLabelText(/^Definition/), { target: { value: "Edited: cheap chatter only" } });
    expect((screen.getByRole("button", { name: "Save tiers" }) as HTMLButtonElement).disabled).toBe(false);

    await fireEvent.click(screen.getByRole("button", { name: "Save tiers" }));
    await waitFor(() => expect(ontiers).toHaveBeenCalledTimes(1));
    // The dialog hands the stored copy back through ontiers + rerender.
    await page.rerender({ tiers: ontiers.mock.calls[0][0] as TiersDto });
    await waitFor(() =>
      expect((screen.getByRole("button", { name: "Save tiers" }) as HTMLButtonElement).disabled).toBe(true),
    );
  });
});
