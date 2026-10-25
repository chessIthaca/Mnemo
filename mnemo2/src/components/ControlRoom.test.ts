import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import ControlRoom from "./ControlRoom.svelte";

const workerRows = () => screen.getAllByRole("button", { name: /Open .* worker thread/ });

describe("Mission Control", () => {
  beforeEach(() => {
    app.set(fixtureState());
    actions.openTeam();
  });

  it("renders ten workers and filters the larger team", async () => {
    render(ControlRoom);
    expect(workerRows()).toHaveLength(10);

    await fireEvent.click(screen.getByRole("button", { name: /Needs you/ }));
    expect(workerRows()).toHaveLength(3);

    await fireEvent.click(screen.getByRole("button", { name: /Done/ }));
    expect(workerRows()).toHaveLength(2);

    await fireEvent.click(screen.getByRole("button", { name: /All/ }));
    expect(workerRows()).toHaveLength(10);
  });

  it("opens a task on its thread with its model and workflow", () => {
    render(ControlRoom);
    expect(screen.getByRole("button", { name: /^Thread/ }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getAllByText("kimi-k3.1-pro").length).toBeGreaterThan(0);
    expect(screen.getByRole("list", { name: /Task workflow:/ })).toBeTruthy();
  });

  it("resets the worker detail to Thread when switching tasks", async () => {
    render(ControlRoom);
    await fireEvent.click(screen.getByRole("button", { name: /^Review/ }));
    expect(screen.getByRole("button", { name: /^Review/ }).getAttribute("aria-pressed")).toBe("true");

    await fireEvent.click(screen.getByRole("button", { name: /Open Theo worker thread/ }));
    expect(screen.getByRole("button", { name: /^Thread/ }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: /^Review/ }).getAttribute("aria-pressed")).toBe("false");
  });

  it("keeps review and merge disabled until the Phase 3 runtime exists", async () => {
    render(ControlRoom);
    await fireEvent.click(screen.getByRole("button", { name: /^Review/ }));
    const merge = screen.getByRole("button", { name: "Merge reviewed branch" }) as HTMLButtonElement;
    expect(merge.disabled).toBe(true);

    await fireEvent.click(
      screen.getByRole("button", { name: "Fix tiny finding: Give the merge action an accessible name" }),
    );
    expect(screen.queryByText("MERGE RECORD")).toBeNull();
    expect(get(app).toast?.message).toMatch(/Phase 3/);
  });

  it("does not expose Diff as a per-worker action", async () => {
    render(ControlRoom);
    await fireEvent.click(screen.getByRole("button", { name: /Open Theo worker thread/ }));
    expect(screen.queryByRole("button", { name: "Diff" })).toBeNull();
  });
});
