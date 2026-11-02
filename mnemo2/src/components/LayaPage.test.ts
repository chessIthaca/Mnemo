import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";
import type { LayaEvent, LayaFlags, LayaStatus, LayaTransport, RoutingPatch } from "../lib/layaSettings";
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

/** The transport fake: a mutable live snapshot, and handlers the test drives. */
function makePage(initial: LayaStatus = sampleStatus()) {
  const status = initial;
  const handlers: ((event: LayaEvent) => void)[] = [];
  const transport = {
    status: vi.fn(async () => structuredClone(status)),
    setup: vi.fn(async () => undefined),
    disable: vi.fn(async () => undefined),
    save: vi.fn(async (_flags?: LayaFlags, _routing?: RoutingPatch) => undefined),
    onLayaEvent: vi.fn(async (handler: (event: LayaEvent) => void) => {
      handlers.push(handler);
      return () => {
        const index = handlers.indexOf(handler);
        if (index >= 0) handlers.splice(index, 1);
      };
    }),
  };
  const emit = (event: LayaEvent): void => {
    for (const handler of [...handlers]) handler(event);
  };
  render(LayaPage, { props: { transport, endpoints: sampleEndpoints() } });
  return { status, transport, emit };
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
    };
    render(LayaPage, { props: { transport, endpoints: sampleEndpoints() } });
    expect(await screen.findByText("config unreadable")).toBeTruthy();
  });
});