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
      model: "test/scripted-a",
      modelId: "scripted-a",
      models: [
        {
          endpoint: "test",
          model: "claude-opus-5-5",
          spec: "test/claude-opus-5-5",
          is_default: true,
        },
        {
          endpoint: "test",
          model: "kimi-k3.1-pro",
          spec: "test/kimi-k3.1-pro",
          is_default: false,
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

  it("shows the configured models and switches on selection", async () => {
    nativeState();
    const setModel = vi.spyOn(actions, "setLeadModel");
    render(LeadModelPicker);
    const select = screen.getByLabelText("Lead model") as HTMLSelectElement;
    expect([...select.options].map((option) => option.textContent)).toEqual([
      "claude-opus-5-5",
      "kimi-k3.1-pro",
    ]);
    await fireEvent.change(select, { target: { value: "test/kimi-k3.1-pro" } });
    expect(setModel).toHaveBeenCalledWith("test/kimi-k3.1-pro");
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
