import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";
import type {
  DispatchRoutingSettings,
  EvalReport,
  LayaEvent,
  LayaFlags,
  LayaStatus,
  LayaTransport,
  RoutingEvalWire,
  RoutingPatch,
  RoutingStatusWire,
} from "../lib/layaSettings";
import { emptyEndpoint, emptyModel, type EndpointDetailDto } from "../lib/settingsEngine";
import LayaPage from "./LayaPage.svelte";

/**
 * The Decisions page against an in-memory LayaTransport: the status
 * block's Enable/Install/Disable lifecycle over the event channel, and
 * the flags/routing sections' own save legs. The wire shapes are the
 * ones src-tauri/src/mnemo2/laya_settings.rs serves.
 */

/** The ready snapshot the page renders, with per-test overrides. */
function sampleStatus(overrides: Partial<LayaStatus> = {}): LayaStatus {
  return {
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
      cheap: "ep/tiny",
      routine: null,
      capable: "ep/big",
      lane_medium: null,
      lane_high: null,
      escalate: null,
      threshold: 0.8,
      enforce: false,
    },
    ...overrides,
  };
}

/** One endpoint with two models, so the pickers have datalist options. */
function sampleEndpoints(): EndpointDetailDto[] {
  const endpoint = { ...emptyEndpoint(), name: "ep", base_url: "https://ep.example/v1/" };
  endpoint.models = [emptyModel("tiny"), emptyModel("big")];
  return [endpoint];
}

/** The Mnemo2 `[routing]` snapshot with only a few shadow decisions. */
function sampleRoutingStatus(overrides: Partial<RoutingStatusWire> = {}): RoutingStatusWire {
  return {
    settings: {
      mode: "off",
      min_confidence_3: 0.475,
      min_confidence_2: 0.6,
      fallback_tier: "average",
      nudges: true,
      min_words: 20,
      effort: {},
    },
    summary: {
      decisions_by_mode: { shadow: 4, off: 2 },
      agreement_rate: 0.5,
      laya_accuracy: 0.75,
      lead_accuracy: 1,
      labelled: 4,
      overrides_by_source: { lead: 1, user: 1 },
      nudges: 2,
      nudges_acted_on: 1,
      eval_date: null,
    },
    eval: null,
    ...overrides,
  };
}

/** One eval report fixture. */
function sampleReport(labels: number, suggested: number | null): EvalReport {
  return {
    labels,
    default_gate: labels === 3 ? 0.475 : 0.6,
    table: [
      { threshold: 0.25, routed: 48, correct: 40, routed_share: 1, accuracy: 40 / 48 },
      { threshold: 0.475, routed: 30, correct: 29, routed_share: 30 / 48, accuracy: 29 / 30 },
    ],
    suggested,
    misses: [{ text: "Make it better.", expected: null, got: "routine", confidence: 0.9 }],
  };
}

/** The `run_routing_eval` fixture. */
function sampleEval(): RoutingEvalWire {
  return {
    three: sampleReport(3, 0.475),
    two: sampleReport(2, 0.5),
    record: { date: "2026-11-03", min_confidence_3: 0.475, min_confidence_2: 0.5, routed_share: 0.625, accuracy: 29 / 30 },
  };
}

/** The transport fake: a mutable live snapshot, and handlers the test drives. */
function makePage(
  initial: LayaStatus = sampleStatus(),
  options: { pendingSetup?: boolean; routing?: RoutingStatusWire } = {},
) {
  const status = initial;
  const routingStatus = options.routing ?? sampleRoutingStatus();
  const handlers: ((event: LayaEvent) => void)[] = [];
  const transport = {
    status: vi.fn(async () => structuredClone(status)),
    // A pending setup pins the pipeline's in-flight window open: the real
    // `laya_setup` returns as soon as the forwarder spawns, and only the
    // channel events settle the run.
    setup: vi.fn(() => (options.pendingSetup ? new Promise<void>(() => {}) : Promise.resolve())),
    disable: vi.fn(async () => undefined),
    save: vi.fn(async (_flags?: LayaFlags, _routing?: RoutingPatch) => undefined),
    onLayaEvent: vi.fn(async (handler: (event: LayaEvent) => void) => {
      handlers.push(handler);
      return () => {
        const index = handlers.indexOf(handler);
        if (index >= 0) handlers.splice(index, 1);
      };
    }),
    routingStatus: vi.fn(async () => structuredClone(routingStatus)),
    saveRoutingSettings: vi.fn(async (settings: DispatchRoutingSettings) => {
      routingStatus.settings = structuredClone(settings);
      return structuredClone(routingStatus);
    }),
    runRoutingEval: vi.fn(async () => sampleEval()),
  };
  const emit = (event: LayaEvent): void => {
    for (const handler of [...handlers]) handler(event);
  };
  render(LayaPage, { props: { transport, endpoints: sampleEndpoints() } });
  return { status, routingStatus, transport, emit };
}

describe("LayaPage", () => {
  it("renders the status block, the seven switches and the routing lanes from the snapshot", async () => {
    makePage();
    const statusBlock = await screen.findByRole("region", { name: "Decision models (Laya)" });
    expect(screen.getByText("Status: Ready")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Disable" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Enable" })).toBeNull();
    expect(screen.getByText(/local logs in the background/)).toBeTruthy();

    const flags = screen.getByRole("region", { name: "What Laya decides" });
    expect((screen.getByRole("checkbox", { name: /Auto-type memories/ }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: /Model routing/ }) as HTMLInputElement).checked).toBe(false);
    expect(within(flags).getAllByRole("checkbox")).toHaveLength(7);

    const lanes = screen.getByRole("region", { name: "Routing lanes" });
    expect((within(lanes).getByLabelText("Cheap lane") as HTMLInputElement).value).toBe("ep/tiny");
    expect((within(lanes).getByLabelText("Capable lane") as HTMLInputElement).value).toBe("ep/big");
    expect((within(lanes).getByLabelText("Escalate") as HTMLInputElement).value).toBe("");
    expect((screen.getByLabelText("Confidence threshold") as HTMLInputElement).value).toBe("0.8");
    expect((screen.getByRole("checkbox", { name: /Enforce decisions/ }) as HTMLInputElement).checked).toBe(false);
    expect(screen.getByText("Off = shadow mode: decisions are logged, not applied.")).toBeTruthy();
  });

  it("saves the seven flags on their own button and notes the new turns", async () => {
    const { transport } = makePage();
    await screen.findByText("Status: Ready");
    await fireEvent.click(screen.getByRole("checkbox", { name: /Model routing/ }));
    await fireEvent.click(screen.getByRole("button", { name: "Save flags" }));
    await waitFor(() => expect(transport.save).toHaveBeenCalledTimes(1));
    const [sentFlags, sentRouting] = transport.save.mock.calls[0] as unknown as [
      Record<string, boolean> | undefined,
      unknown,
    ];
    expect(sentFlags).toEqual({
      auto_type_memories: true,
      failure_triage: false,
      failure_triage_knn: false,
      auto_finetune: false,
      steer_tool_choice: false,
      routing: true,
      reflex: false,
    });
    expect(sentRouting).toBeUndefined();
    expect(await screen.findByText("Applies to new turns.")).toBeTruthy();
  });

  it("saves routing with endpoint/model wire refs, clearing emptied lanes to null", async () => {
    const { transport } = makePage();
    await screen.findByText("Status: Ready");
    await fireEvent.input(screen.getByLabelText("Cheap lane"), { target: { value: "" } });
    await fireEvent.input(screen.getByLabelText("Escalate"), { target: { value: "ep/big" } });
    await fireEvent.input(screen.getByLabelText("Confidence threshold"), { target: { value: "0.95" } });
    await fireEvent.click(screen.getByRole("checkbox", { name: /Enforce decisions/ }));
    await fireEvent.click(screen.getByRole("button", { name: "Save routing" }));
    await waitFor(() => expect(transport.save).toHaveBeenCalledTimes(1));
    const [sentFlags, sentRouting] = transport.save.mock.calls[0] as unknown as [
      unknown,
      Record<string, unknown> | undefined,
    ];
    expect(sentFlags).toBeUndefined();
    expect(sentRouting).toEqual({
      cheap: null,
      routine: null,
      capable: { endpoint: "ep", model: "big" },
      lane_medium: null,
      lane_high: null,
      escalate: { endpoint: "ep", model: "big" },
      threshold: 0.95,
      enforce: true,
    });
    expect(await screen.findByText("Applies to new turns.")).toBeTruthy();
  });

  it("rejects a malformed lane ref and a bad threshold inline without saving", async () => {
    const { transport } = makePage();
    await screen.findByText("Status: Ready");
    await fireEvent.input(screen.getByLabelText("Cheap lane"), { target: { value: "no-slash" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save routing" }));
    expect(await screen.findByText('Cheap lane model must be "endpoint/model"')).toBeTruthy();
    expect(transport.save).not.toHaveBeenCalled();
    await fireEvent.input(screen.getByLabelText("Cheap lane"), { target: { value: "ep/tiny" } });
    await fireEvent.input(screen.getByLabelText("Confidence threshold"), { target: { value: "9" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save routing" }));
    expect(await screen.findByText("Threshold must be a number from 0 to 1")).toBeTruthy();
    expect(transport.save).not.toHaveBeenCalled();
  });

  it("enables Laya and follows the install progress to ready", async () => {
    const { status, transport, emit } = makePage(sampleStatus({ enabled: false, installed: false, status: "disabled" }));
    expect(await screen.findByText("Status: Off")).toBeTruthy();
    expect(screen.getByText(/no classifier calls, no server/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Enable" }));
    await waitFor(() => expect(transport.setup).toHaveBeenCalledTimes(1));
    emit({ type: "progress", step: "laya_runtime", label: "Downloading the English checkpoint", fraction: 0.62 });
    const bar = await screen.findByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBe("62");
    expect(screen.getByText("Downloading the English checkpoint")).toBeTruthy();
    status.enabled = true;
    status.installed = true;
    status.status = "ready";
    emit({ type: "done", step: "laya_runtime" });
    await screen.findByText("Status: Ready");
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.getByRole("button", { name: "Disable" })).toBeTruthy();
  });

  it("shows Installing, not Off, on the stale snapshot while the setup pipeline runs", async () => {
    // Red check for review finding statusOffWhileInstalling: the snapshot
    // is re-read only when the pipeline settles, so mid-flight the status
    // line must show the pipeline, not the stale pre-click "Off".
    const { transport, emit } = makePage(sampleStatus({ enabled: false, installed: false, status: "disabled" }), {
      pendingSetup: true,
    });
    expect(await screen.findByText("Status: Off")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Enable" }));
    await waitFor(() => expect(transport.setup).toHaveBeenCalledTimes(1));
    emit({ type: "progress", step: "laya_runtime", label: "Installing laya[serve]", fraction: 0.5 });
    expect(await screen.findByText("Status: Installing…")).toBeTruthy();
    expect(screen.queryByText("Status: Off")).toBeNull();
    expect(screen.getByText("Downloading the managed runtime — the bar follows its progress.")).toBeTruthy();
    expect(screen.getByText("Installing laya[serve]")).toBeTruthy();
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("50");
  });

  it("shows an indeterminate progress without a bar, and a failed setup inline", async () => {
    const { status, emit } = makePage(sampleStatus({ enabled: false, installed: false, status: "disabled" }));
    await screen.findByText("Status: Off");
    await fireEvent.click(screen.getByRole("button", { name: "Enable" }));
    emit({ type: "progress", step: "laya_runtime", label: "Downloading the runtime", fraction: null });
    expect(await screen.findByText("Downloading the runtime")).toBeTruthy();
    // Indeterminate: the label without a bar.
    expect(screen.queryByRole("progressbar")).toBeNull();
    status.status = "failed";
    emit({ type: "failed", step: "laya_runtime", error: "download interrupted" });
    await screen.findByText("download interrupted");
    await screen.findByText("Status: Failed");
    expect(screen.getByRole("button", { name: "Enable" })).toBeTruthy();
  });

  it("disables Laya and refreshes the status", async () => {
    const { status, transport } = makePage();
    await screen.findByText("Status: Ready");
    status.enabled = false;
    status.status = "disabled";
    await fireEvent.click(screen.getByRole("button", { name: "Disable" }));
    await waitFor(() => expect(transport.disable).toHaveBeenCalledTimes(1));
    await screen.findByText("Status: Off");
    expect(screen.getByRole("button", { name: "Enable" })).toBeTruthy();
  });

  it("offers Install beside Disable when enabled but not installed", async () => {
    makePage(sampleStatus({ installed: false, status: "not_installed" }));
    await screen.findByText("Status: Not installed");
    expect(screen.getByRole("button", { name: "Install" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Disable" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Enable" })).toBeNull();
  });

  it("keeps an unsaved flags edit across the install refresh", async () => {
    const { status, emit } = makePage(sampleStatus({ enabled: false, installed: false, status: "disabled" }));
    await screen.findByText("Status: Off");
    await fireEvent.click(screen.getByRole("checkbox", { name: /Failure triage/ }));
    status.enabled = true;
    status.installed = true;
    status.status = "ready";
    emit({ type: "done", step: "laya_runtime" });
    await screen.findByText("Status: Ready");
    expect((screen.getByRole("checkbox", { name: /Failure triage/ }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("button", { name: "Save flags" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("shows the load error when the status read fails", async () => {
    const transport: LayaTransport = {
      status: vi.fn(async () => Promise.reject("config unreadable")),
      setup: vi.fn(async () => undefined),
      disable: vi.fn(async () => undefined),
      save: vi.fn(async () => undefined),
      onLayaEvent: vi.fn(async () => () => {}),
      routingStatus: vi.fn(async () => sampleRoutingStatus()),
      saveRoutingSettings: vi.fn(async () => sampleRoutingStatus()),
      runRoutingEval: vi.fn(async () => sampleEval()),
    };
    render(LayaPage, { props: { transport, endpoints: sampleEndpoints() } });
    expect(await screen.findByText("config unreadable")).toBeTruthy();
  });

  it("renders the four dispatch modes, locking suggest and auto under 30 labelled decisions", async () => {
    makePage();
    const block = await screen.findByRole("region", { name: "Worker dispatch routing" });
    expect(within(block).getAllByRole("radio")).toHaveLength(4);
    expect((within(block).getByRole("radio", { name: /^Off / }) as HTMLInputElement).checked).toBe(true);
    expect((within(block).getByRole("radio", { name: /^Shadow / }) as HTMLInputElement).disabled).toBe(false);
    expect((within(block).getByRole("radio", { name: /^Suggest / }) as HTMLInputElement).disabled).toBe(true);
    expect((within(block).getByRole("radio", { name: /^Auto / }) as HTMLInputElement).disabled).toBe(true);
    const forced = within(block).getAllByRole("button", { name: /^force \(/ });
    expect(forced).toHaveLength(2);
    expect(forced[0].textContent).toContain("4 of 30 shadow decisions");
    expect(screen.getAllByText(/provisional defaults, no eval yet/).length).toBe(2);
  });

  it("saves the [routing] table through saveRoutingSettings", async () => {
    const { transport } = makePage();
    const block = await screen.findByRole("region", { name: "Worker dispatch routing" });
    await fireEvent.input(within(block).getByLabelText("Three-label gate"), { target: { value: "0.5" } });
    await fireEvent.click(within(block).getByRole("button", { name: "Save dispatch routing" }));
    await waitFor(() => expect(transport.saveRoutingSettings).toHaveBeenCalledTimes(1));
    const sent = transport.saveRoutingSettings.mock.calls[0][0] as unknown as DispatchRoutingSettings;
    expect(sent.mode).toBe("off");
    expect(sent.min_confidence_3).toBe(0.5);
    expect(sent.fallback_tier).toBe("average");
    expect(await within(block).findByText("Saved to mnemo2.toml.")).toBeTruthy();
  });

  it("saves a picked mode at once, without the Save button", async () => {
    const { transport } = makePage();
    const block = await screen.findByRole("region", { name: "Worker dispatch routing" });
    await fireEvent.click(within(block).getByRole("radio", { name: /^Shadow / }));
    await waitFor(() => expect(transport.saveRoutingSettings).toHaveBeenCalledTimes(1));
    expect(transport.saveRoutingSettings.mock.calls[0][0]).toEqual({
      mode: "shadow",
      min_confidence_3: 0.475,
      min_confidence_2: 0.6,
      fallback_tier: "average",
      nudges: true,
      min_words: 20,
      effort: {},
    });
    expect((within(block).getByRole("radio", { name: /^Shadow / }) as HTMLInputElement).checked).toBe(true);
    expect(await within(block).findByText("Saved to mnemo2.toml.")).toBeTruthy();
    // The section came back clean: the Save button goes quiet.
    expect(
      (within(block).getByRole("button", { name: "Save dispatch routing" }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("reverts the radio and shows the error when a picked mode fails to save", async () => {
    const { transport } = makePage();
    transport.saveRoutingSettings.mockRejectedValueOnce(new Error("mnemo2.toml: disk full"));
    const block = await screen.findByRole("region", { name: "Worker dispatch routing" });
    await fireEvent.click(within(block).getByRole("radio", { name: /^Shadow / }));
    expect(await within(block).findByText("mnemo2.toml: disk full")).toBeTruthy();
    expect((within(block).getByRole("radio", { name: /^Off / }) as HTMLInputElement).checked).toBe(true);
    expect((within(block).getByRole("radio", { name: /^Shadow / }) as HTMLInputElement).checked).toBe(false);
    expect(transport.saveRoutingSettings).toHaveBeenCalledTimes(1);
    expect(within(block).queryByText("Saved to mnemo2.toml.")).toBeNull();
  });

  it("keeps an unsaved gate edit out of the mode save, then still saves it on its own", async () => {
    const { transport } = makePage();
    const block = await screen.findByRole("region", { name: "Worker dispatch routing" });
    await fireEvent.input(within(block).getByLabelText("Three-label gate"), { target: { value: "0.5" } });
    await fireEvent.click(within(block).getByRole("radio", { name: /^Shadow / }));
    await waitFor(() => expect(transport.saveRoutingSettings).toHaveBeenCalledTimes(1));
    const modeSave = transport.saveRoutingSettings.mock.calls[0][0] as unknown as DispatchRoutingSettings;
    expect(modeSave.mode).toBe("shadow");
    // The gate rides the saved settings, not the draft's unsaved 0.5.
    expect(modeSave.min_confidence_3).toBe(0.475);
    // The draft keeps its unsaved edit: the gate still reads 0.5, the Save
    // button stays live once the mode save settles, and the note stays honest.
    expect((within(block).getByLabelText("Three-label gate") as HTMLInputElement).value).toBe("0.5");
    const saveButton = within(block).getByRole("button", { name: "Save dispatch routing" }) as HTMLButtonElement;
    await waitFor(() => expect(saveButton.disabled).toBe(false));
    expect(within(block).queryByText("Saved to mnemo2.toml.")).toBeNull();
    await fireEvent.click(saveButton);
    await waitFor(() => expect(transport.saveRoutingSettings).toHaveBeenCalledTimes(2));
    const tableSave = transport.saveRoutingSettings.mock.calls[1][0] as unknown as DispatchRoutingSettings;
    expect(tableSave.mode).toBe("shadow");
    expect(tableSave.min_confidence_3).toBe(0.5);
    expect(await within(block).findByText("Saved to mnemo2.toml.")).toBeTruthy();
  });

  it("runs the eval and shows the table, the misses and the suggested gates", async () => {
    const { transport } = makePage();
    const block = await screen.findByRole("region", { name: "Worker dispatch routing" });
    await fireEvent.click(within(block).getByRole("button", { name: "Run eval" }));
    await waitFor(() => expect(transport.runRoutingEval).toHaveBeenCalledTimes(1));
    expect(await within(block).findByText(/Three labels — suggested 0\.475/)).toBeTruthy();
    expect(within(block).getByText(/Two labels — suggested 0\.500/)).toBeTruthy();
    expect(within(block).getAllByText(/want no route · got routine/).length).toBeGreaterThan(0);
    expect(within(block).getByRole("button", { name: "Apply suggested gates" })).toBeTruthy();
  });

  it("asks before a navigation drops unsaved dispatch edits, and a decline cancels it", async () => {
    const confirmSpy = vi.fn();
    vi.stubGlobal("confirm", confirmSpy);
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    const heard = vi.fn();
    outside.addEventListener("click", heard);
    try {
      makePage();
      const block = await screen.findByRole("region", { name: "Worker dispatch routing" });
      await fireEvent.input(within(block).getByLabelText("Three-label gate"), { target: { value: "0.5" } });
      // The guard is armed once the dirty flag renders.
      const saveButton = within(block).getByRole("button", { name: "Save dispatch routing" }) as HTMLButtonElement;
      await waitFor(() => expect(saveButton.disabled).toBe(false));
      // In-app navigation: the declined click dies before the outside button hears it.
      await fireEvent.click(outside);
      await waitFor(() => expect(confirmSpy).toHaveBeenCalledTimes(1));
      expect(heard).not.toHaveBeenCalled();
      // Escape asks too, and a decline keeps the dialog open.
      const escaped = vi.fn();
      window.addEventListener("keydown", escaped);
      await fireEvent.keyDown(document.body, { key: "Escape" });
      await waitFor(() => expect(confirmSpy).toHaveBeenCalledTimes(2));
      expect(escaped).not.toHaveBeenCalled();
      window.removeEventListener("keydown", escaped);
      // Window unload rides the browser's own prompt.
      const unload = new Event("beforeunload", { cancelable: true });
      window.dispatchEvent(unload);
      expect(unload.defaultPrevented).toBe(true);
    } finally {
      outside.remove();
      vi.unstubAllGlobals();
    }
  });

  it("lets navigation through when the user accepts, and never asks while clean", async () => {
    const confirmSpy = vi.fn();
    vi.stubGlobal("confirm", confirmSpy);
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    const heard = vi.fn();
    outside.addEventListener("click", heard);
    try {
      makePage();
      const block = await screen.findByRole("region", { name: "Worker dispatch routing" });
      // Clean: the outside click needs no leave-taking.
      await fireEvent.click(outside);
      expect(confirmSpy).not.toHaveBeenCalled();
      expect(heard).toHaveBeenCalledTimes(1);
      // Dirty but accepted: the navigation runs.
      await fireEvent.input(within(block).getByLabelText("Three-label gate"), { target: { value: "0.5" } });
      const saveButton = within(block).getByRole("button", { name: "Save dispatch routing" }) as HTMLButtonElement;
      await waitFor(() => expect(saveButton.disabled).toBe(false));
      await fireEvent.click(outside);
      await waitFor(() => expect(confirmSpy).toHaveBeenCalledTimes(1));
      expect(heard).toHaveBeenCalledTimes(1);
      confirmSpy.mockReturnValue(true);
      await fireEvent.click(outside);
      await waitFor(() => expect(heard).toHaveBeenCalledTimes(2));
      expect(confirmSpy).toHaveBeenCalledTimes(2);
    } finally {
      outside.remove();
      vi.unstubAllGlobals();
    }
  });
});