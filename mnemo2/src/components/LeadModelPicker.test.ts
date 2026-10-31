import { fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import { initialLeadRuntime } from "../lib/leadState";
import LeadModelPicker from "./LeadModelPicker.svelte";

function nativeState(busy = false) {
  app.update((state) => ({
    ...state,
    projectId: "mnemo",
    leadRuntime: {
      ...initialLeadRuntime(),
      mode: "native",
      status: "ready",
      generation: 1,
      busy,
      model: "test/claude-opus-5-5",
      modelId: "claude-opus-5-5",
      models: [
        {
          endpoint: "test",
          model: "claude-opus-5-5",
          spec: "test/claude-opus-5-5",
          is_default: true,
          reasoning_efforts: ["max", "high", "medium", "low", "minimal", "off"],
          default_effort: "max",
        },
        {
          endpoint: "test",
          model: "kimi-k3.1-pro",
          spec: "test/kimi-k3.1-pro",
          is_default: false,
          reasoning_efforts: ["high", "low", "off"],
          default_effort: "high",
        },
        {
          endpoint: "other",
          model: "kimi-k3.1-pro",
          spec: "other/kimi-k3.1-pro",
          is_default: false,
          reasoning_efforts: [],
          default_effort: null,
        },
      ],
    },
  }));
}

describe("LeadModelPicker", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("names every model by endpoint so equal model ids stay distinct", async () => {
    nativeState();
    const setModel = vi.spyOn(actions, "setLeadModel").mockImplementation(() => {});
    render(LeadModelPicker);
    const select = screen.getByLabelText("Lead model") as HTMLSelectElement;
    expect([...select.options].map((option) => option.textContent)).toEqual([
      "test / claude-opus-5-5 (default)",
      "test / kimi-k3.1-pro",
      "other / kimi-k3.1-pro",
    ]);
    await fireEvent.change(select, { target: { value: "other/kimi-k3.1-pro" } });
    expect(setModel).toHaveBeenCalledWith("other/kimi-k3.1-pro", null);
  });

  it("offers the selected model's reasoning efforts and switches effort", async () => {
    nativeState();
    const setModel = vi.spyOn(actions, "setLeadModel").mockImplementation(() => {});
    render(LeadModelPicker);
    const effort = screen.getByLabelText("Lead reasoning effort") as HTMLSelectElement;
    expect([...effort.options].map((option) => option.textContent)).toEqual([
      "effort: default (max)",
      "effort: max",
      "effort: high",
      "effort: medium",
      "effort: low",
      "effort: minimal",
      "effort: off",
    ]);
    await fireEvent.change(effort, { target: { value: "low" } });
    expect(setModel).toHaveBeenCalledWith("test/claude-opus-5-5", "low");
  });

  it("keeps a supported effort across a model change and drops an unsupported one", async () => {
    nativeState();
    app.update((state) => ({ ...state, leadRuntime: { ...state.leadRuntime, reasoningEffort: "low" } }));
    const setModel = vi.spyOn(actions, "setLeadModel").mockImplementation(() => {});
    render(LeadModelPicker);
    const select = screen.getByLabelText("Lead model") as HTMLSelectElement;
    await fireEvent.change(select, { target: { value: "test/kimi-k3.1-pro" } });
    expect(setModel).toHaveBeenLastCalledWith("test/kimi-k3.1-pro", "low");
    app.update((state) => ({ ...state, leadRuntime: { ...state.leadRuntime, reasoningEffort: "medium" } }));
    await fireEvent.change(select, { target: { value: "test/kimi-k3.1-pro" } });
    expect(setModel).toHaveBeenLastCalledWith("test/kimi-k3.1-pro", null);
  });

  it("hides the effort control for a model that takes no reasoning effort", () => {
    nativeState();
    app.update((state) => ({ ...state, leadRuntime: { ...state.leadRuntime, model: "other/kimi-k3.1-pro" } }));
    render(LeadModelPicker);
    expect(screen.queryByLabelText("Lead reasoning effort")).toBeNull();
  });

  it("disables model changes while the lead is working", () => {
    nativeState(true);
    render(LeadModelPicker);
    const select = screen.getByLabelText("Lead model") as HTMLSelectElement;
    expect(select.disabled).toBe(true);
  });

  it("disables model changes while an interaction is pending", () => {
    nativeState();
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: { kind: "approval", request_id: "req-1" },
      },
    }));
    render(LeadModelPicker);
    const select = screen.getByLabelText("Lead model") as HTMLSelectElement;
    expect(select.disabled).toBe(true);
  });

  it("falls back to the model badge in browser mode", () => {
    actions.openProject("mnemo");
    render(LeadModelPicker);
    expect(screen.queryByLabelText("Lead model")).toBeNull();
    expect(screen.getByLabelText(/Lead model:/)).toBeTruthy();
  });
});
