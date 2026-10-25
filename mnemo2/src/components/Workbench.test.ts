import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import Workbench from "./Workbench.svelte";

const state = () => get(app);

describe("Workbench", () => {
  beforeEach(() => {
    const state = fixtureState();
    state.plan = {
      id: "contract",
      title: "Define the lead/team contract",
      goal: "Build the actual lead plan view",
      steps: state.planItems.map((step, index) => ({
        index,
        text: step.detail,
        header: step.title,
        done: step.status === "done",
      })),
    };
    app.set(state);
    actions.openWorkbench("diff");
  });

  it("switches between the Diff, Plan, and Browser artifacts", async () => {
    render(Workbench);
    expect(screen.getByText("mnemo2/src/lib/review.ts")).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Plan" }));
    expect(await screen.findByRole("heading", { name: "Define the lead/team contract", level: 2 })).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Browser" }));
    expect(screen.getByRole("textbox", { name: "Address" })).toBeTruthy();
  });

  it("opens a lead-chat link in the browser and normalizes addresses", async () => {
    actions.openBrowser("https://example.com/docs");
    render(Workbench);

    const address = screen.getByRole("textbox", { name: "Address" }) as HTMLInputElement;
    expect(address.value).toBe("https://example.com/docs");

    await fireEvent.input(address, { target: { value: "example.com/notes" } });
    app.update((state) => ({ ...state, browserSupported: false }));
    expect(address.value).toBe("example.com/notes");
    await fireEvent.submit(address.closest("form")!);
    await vi.waitFor(() => expect(state().browserUrl).toBe("https://example.com/notes"));
  });
});
