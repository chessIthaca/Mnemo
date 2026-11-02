import { fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import ProjectPage from "./ProjectPage.svelte";

const slotsInput = () => screen.getByLabelText("Project worker slots") as HTMLInputElement;

describe("ProjectPage", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("shows the open project's worker slots and sends a change through the store action", async () => {
    const setSlots = vi.spyOn(actions, "setWorkerSlots").mockImplementation(() => {});
    render(ProjectPage);

    expect(slotsInput().value).toBe("8");

    await fireEvent.change(slotsInput(), { target: { value: "5" } });

    expect(setSlots).toHaveBeenCalledWith(5);
  });

  it("shows the lead model picker and the merge policy the app enforces", () => {
    render(ProjectPage);

    expect(screen.getByLabelText("Lead model: claude-opus-5-5")).toBeTruthy();
    expect(screen.getByText("Merge policy")).toBeTruthy();
    expect(screen.getByText("Guarded")).toBeTruthy();
  });

  it("says a project must be open when there is none", () => {
    app.set({ ...fixtureState(), projectId: null });
    render(ProjectPage);

    expect(screen.getByText("Open a project to change its settings.")).toBeTruthy();
    expect(screen.queryByLabelText("Project worker slots")).toBeNull();
  });
});
