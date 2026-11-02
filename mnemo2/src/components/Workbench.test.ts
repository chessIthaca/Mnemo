import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import Workbench from "./Workbench.svelte";

const state = () => get(app);

describe("Workbench", () => {
  beforeEach(() => {
    const state = fixtureState();
    state.board = [{
      id: "contract",
      title: "Define the lead/team contract",
      goal: "Build the board view",
      kind: "feature",
      status: "open",
      closed_at_ms: null,
      report_path: null,
      steps: [{ number: 1, header: "Contract", text: "Write it", done: false, workers: [] }],
    }];
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

  it("routes plan-goal links through the workbench browser", async () => {
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) =>
      window.setTimeout(callback, 0));
    vi.stubGlobal("cancelAnimationFrame", (id: number) => window.clearTimeout(id));
    const current = fixtureState();
    current.board = [{
      id: "contract",
      title: "Plan",
      goal: "Read [Docs](https://example.com/docs)",
      kind: "feature",
      status: "open",
      closed_at_ms: null,
      report_path: null,
      steps: [],
    }];
    app.set(current);
    actions.openWorkbench("plan");
    render(Workbench);

    const link = await screen.findByRole("link", { name: "Docs" }, { timeout: 3000 });
    await fireEvent.click(link);
    expect(state().workbenchTab).toBe("browser");
    expect(state().browserUrl).toBe("https://example.com/docs");
  });

  it("shows only open plans in its Plan tab and links to the full list", async () => {
    const current = fixtureState();
    current.board = [
      {
        id: "contract",
        title: "Open contract",
        goal: "",
        kind: "feature",
        status: "open",
        closed_at_ms: null,
        report_path: null,
        steps: [],
      },
      {
        id: "done",
        title: "Finished plan",
        goal: "",
        kind: "bug_fixing",
        status: "closed",
        closed_at_ms: 5,
        report_path: null,
        steps: [],
      },
    ];
    app.set(current);
    actions.openWorkbench("plan");
    const { container } = render(Workbench);

    expect(await screen.findByRole("heading", { name: "Open contract", level: 2 })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Finished plan" })).toBeNull();
    expect(container.querySelector("details.closed")).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: "All plans" }));
    expect(state().route).toBe("plans");
    expect(state().workbenchOpen).toBe(false);
  });

  it("gives width back to the lead when the window shrinks", async () => {
    const original = Object.getOwnPropertyDescriptor(window, "innerWidth");
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1600 });
    try {
      actions.setWorkbenchWidth(1100);
      render(Workbench);
      expect(state().workbenchWidth).toBe(1100);

      Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1280 });
      await fireEvent(window, new Event("resize"));
      await vi.waitFor(() => expect(state().workbenchWidth).toBe(860));
    } finally {
      if (original) Object.defineProperty(window, "innerWidth", original);
    }
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});
