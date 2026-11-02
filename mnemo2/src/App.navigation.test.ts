import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
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

  it("opens the Cost and Graph tabs from the top bar", async () => {
    render(App);

    await fireEvent.click(screen.getByRole("button", { name: /^Cost/ }));
    expect(state().route).toBe("cost");
    expect(await screen.findByRole("region", { name: "Cost" })).toBeTruthy();
    expect(screen.queryByRole("textbox", { name: "Message the lead" })).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: /^Graph/ }));
    expect(state().route).toBe("graph");
    expect(await screen.findByRole("region", { name: "Code graph" })).toBeTruthy();
  });

  it("opens the Cost tab from the cost chip", async () => {
    render(App);

    await fireEvent.click(screen.getByRole("button", { name: /Project cost/ }));

    expect(state().route).toBe("cost");
    expect(await screen.findByRole("region", { name: "Cost" })).toBeTruthy();
    expect(screen.queryByRole("dialog", { name: "Project settings" })).toBeNull();
  });

  it("opens the Trace tab from the top bar between Cost and Graph", async () => {
    render(App);

    await fireEvent.click(screen.getByRole("button", { name: /^Trace/ }));

    expect(state().route).toBe("trace");
    expect(await screen.findByRole("region", { name: "Trace" })).toBeTruthy();
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
