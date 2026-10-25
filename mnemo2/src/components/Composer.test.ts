import { fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import { initialLeadRuntime } from "../lib/leadState";
import Composer from "./Composer.svelte";

function setLead(busy: boolean, status: "ready" | "opening" = "ready") {
  app.update((state) => ({
    ...state,
    projectId: "mnemo",
    leadRuntime: {
      ...initialLeadRuntime(),
      mode: "native",
      status,
      generation: 1,
      busy,
      model: "test/scripted-a",
      modelId: "scripted-a",
      models: [
        {
          endpoint: "test",
          model: "scripted-a",
          spec: "test/scripted-a",
          is_default: true,
        },
      ],
    },
  }));
}

describe("Composer", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("offers a stop control while the lead is working", async () => {
    setLead(true);
    const stop = vi.spyOn(actions, "interruptLead");
    render(Composer);
    expect(screen.queryByRole("button", { name: "Send message" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Stop the lead" }));
    expect(stop).toHaveBeenCalled();
  });

  it("disables send until a native lead is ready", () => {
    setLead(false, "opening");
    render(Composer);
    const input = screen.getByRole("textbox", { name: "Message the lead" });
    expect(input).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("disables send while an interaction is pending", () => {
    setLead(false);
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: { kind: "approval", request_id: "req-1" },
      },
    }));
    render(Composer);
    expect(
      (screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });
});
