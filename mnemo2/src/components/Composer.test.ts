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

  it("keeps stop available while an interaction is pending", async () => {
    setLead(false);
    const stop = vi.spyOn(actions, "interruptLead").mockResolvedValue(undefined);
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: { kind: "approval", request_id: "req-1" },
      },
    }));
    render(Composer);
    expect(screen.queryByRole("button", { name: "Send message" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Stop the lead" }));
    expect(stop).toHaveBeenCalled();
  });

  it("keeps Stopping visible until the engine confirms completion", async () => {
    setLead(true);
    let finish!: () => void;
    vi.spyOn(actions, "interruptLead").mockImplementation(
      () => new Promise<void>((resolve) => { finish = resolve; }),
    );
    render(Composer);
    await fireEvent.click(screen.getByRole("button", { name: "Stop the lead" }));
    const stopping = screen.getByRole("button", { name: "Stopping the lead" }) as HTMLButtonElement;
    expect(stopping.disabled).toBe(true);

    finish();
    await vi.waitFor(() => expect(stopping.disabled).toBe(true));
    app.update((state) => ({
      ...state,
      leadRuntime: { ...state.leadRuntime, busy: false },
    }));
    expect(await screen.findByRole("button", { name: "Send message" })).toBeTruthy();
  });
});
