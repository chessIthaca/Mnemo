import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App.svelte";
import { actions, app } from "./lib/store";
import { fixtureState } from "./test/fixtures";

const state = () => get(app);
const textbox = async () =>
  (await screen.findByRole("textbox", { name: "Message the lead" })) as HTMLTextAreaElement;

describe("Mnemo2 navigation and draft preservation", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  it("opens Mission Control directly from the Team tab with no peek step", async () => {
    app.update((current) => ({ ...current, teamPeekOpen: true }));
    render(App);

    await fireEvent.click(screen.getByRole("button", { name: /^Team/ }));

    expect(state().route).toBe("team");
    expect(state().teamPeekOpen).toBe(false);
    expect(screen.queryByRole("complementary", { name: "Team peek" })).toBeNull();
    expect(
      await screen.findByRole("region", { name: "Mission Control" }),
    ).toBeTruthy();
  });

  it("opens the Cost tab from the top bar", async () => {
    render(App);

    await fireEvent.click(screen.getByRole("button", { name: /^Cost/ }));
    expect(state().route).toBe("cost");
    expect(await screen.findByRole("region", { name: "Cost" })).toBeTruthy();
    expect(screen.queryByRole("textbox", { name: "Message the lead" })).toBeNull();
  });

  it("opens the Cost tab from the cost chip", async () => {
    render(App);

    await fireEvent.click(screen.getByRole("button", { name: /Project cost/ }));

    expect(state().route).toBe("cost");
    expect(await screen.findByRole("region", { name: "Cost" })).toBeTruthy();
    expect(screen.queryByRole("dialog", { name: "Project settings" })).toBeNull();
  });

  it("restores an unsent text and image draft across Lead → Team → Lead", async () => {
    render(App);
    const input = await textbox();
    await fireEvent.input(input, { target: { value: "keep this exact draft" } });
    actions.addLeadDraftImage("data:image/png;base64,kept");
    await vi.waitFor(() => expect(screen.getByAltText("Attachment 1")).toBeTruthy());

    await fireEvent.click(screen.getByRole("button", { name: /^Team/ }));
    expect(screen.queryByRole("textbox", { name: "Message the lead" })).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: /^Lead/ }));
    expect((await textbox()).value).toBe("keep this exact draft");
    expect(await screen.findByAltText("Attachment 1")).toBeTruthy();
  });

  it("starts with an empty draft after opening another project", async () => {
    app.set({
      ...fixtureState(),
      leadDraft: {
        text: "old project draft",
        images: ["data:image/png;base64,old"],
      },
    });
    render(App);

    await actions.openProject("garden-game");

    expect(state().projectId).toBe("garden-game");
    expect(state().leadDraft).toEqual({ text: "", images: [] });
    expect((await textbox()).value).toBe("");
    expect(screen.queryByAltText("Attachment 1")).toBeNull();
  });
});

describe("the side panel on every main tab", () => {
  const originalWidth = window.innerWidth;

  /** jsdom reads its viewport from `window.innerWidth`, so tests pin it explicitly. */
  function pinWidth(width: number) {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
  }

  beforeEach(() => {
    pinWidth(1600);
    app.set(fixtureState());
  });

  afterEach(() => {
    pinWidth(originalWidth);
  });

  it("offers only Lead, Team, Cost and Plans as main tabs", () => {
    render(App);

    const nav = within(screen.getByRole("navigation", { name: "Workspace" }));
    const labels = nav.getAllByRole("button").map((button) => button.textContent?.trim() ?? "");

    expect(labels).toHaveLength(4);
    ["Lead", "Team", "Cost", "Plans"].forEach((name, index) => {
      expect(labels[index].startsWith(name)).toBe(true);
    });
  });

  it.each(["lead", "team", "cost", "plans"] as const)(
    "opens the panel beside the %s tab without leaving it",
    async (route) => {
      app.set({ ...fixtureState(), route });
      const { container } = render(App);

      await fireEvent.click(screen.getByRole("button", { name: "Open workbench" }));

      expect(container.querySelector(".workbench")).not.toBeNull();
      expect(state().route).toBe(route);
      expect(container.querySelector(".main-route")?.firstElementChild).not.toBeNull();
    },
  );

  it("keeps the panel open across main-tab switches", async () => {
    const { container } = render(App);
    await fireEvent.click(screen.getByRole("button", { name: "Open workbench" }));

    for (const name of [/^Team/, /^Cost/, /^Plans/, /^Lead/]) {
      await fireEvent.click(screen.getByRole("button", { name }));
      expect(container.querySelector(".workbench")).not.toBeNull();
    }
  });

  it("reopens the panel on the tab it was left on", async () => {
    render(App);
    actions.openWorkbench("memory");
    await vi.waitFor(() => expect(state().workbenchOpen).toBe(true));

    // Both the top bar and the panel's header carry the close control.
    await fireEvent.click(screen.getAllByRole("button", { name: "Close workbench" })[0]);
    expect(state().workbenchOpen).toBe(false);

    await fireEvent.click(screen.getByRole("button", { name: "Open workbench" }));

    expect(state().workbenchTab).toBe("memory");
  });

  it("shows Memory, Graph and Trace in the panel from any main tab", async () => {
    app.set({ ...fixtureState(), route: "team" });
    render(App);
    actions.openWorkbench("memory");
    expect(await screen.findByRole("region", { name: "Memory" })).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Graph" }));
    expect(await screen.findByRole("region", { name: "Code graph" }, { timeout: 15000 })).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Trace" }));
    expect(await screen.findByRole("region", { name: "Trace" }, { timeout: 15000 })).toBeTruthy();

    expect(state().route).toBe("team");
    expect(screen.getByRole("region", { name: "Mission Control" })).toBeTruthy();
  });

  it("closes the covering panel when a main tab is chosen in a narrow window", async () => {
    pinWidth(900);
    render(App);
    await fireEvent.click(screen.getByRole("button", { name: "Open workbench" }));
    expect(state().workbenchOpen).toBe(true);

    await fireEvent.click(screen.getByRole("button", { name: /^Team/ }));

    expect(state().workbenchOpen).toBe(false);
    expect(state().route).toBe("team");
  });

  it("keeps the worker panel on the Lead tab only", async () => {
    const { container } = render(App);
    actions.openWorkerPanel("ana");
    await vi.waitFor(() => expect(container.querySelector(".worker-panel")).not.toBeNull());

    await fireEvent.click(screen.getByRole("button", { name: /^Team/ }));
    expect(container.querySelector(".worker-panel")).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: /^Lead/ }));
    await vi.waitFor(() => expect(container.querySelector(".worker-panel")).not.toBeNull());
  });
});
