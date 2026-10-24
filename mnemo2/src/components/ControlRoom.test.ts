import { fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it } from "vitest";
import { actions } from "../lib/store";
import ControlRoom from "./ControlRoom.svelte";

const workerRows = () => screen.getAllByRole("button", { name: /Open .* worker thread/ });

describe("Mission Control", () => {
  beforeEach(() => {
    actions.resetDemo();
    actions.openProject("mnemo");
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

  it("drives finding resolution through to an enabled merge and record", async () => {
    render(ControlRoom);
    expect((screen.getByRole("button", { name: "Merge reviewed branch" }) as HTMLButtonElement).disabled).toBe(true);

    await fireEvent.click(screen.getByRole("button", { name: "Fix tiny finding: Give the merge action an accessible name" }));
    await fireEvent.click(screen.getByRole("button", { name: "Spawn fixer: Recheck the tip after a fixer commit" }));
    expect((screen.getByRole("button", { name: "Merge reviewed branch" }) as HTMLButtonElement).disabled).toBe(true);

    await fireEvent.click(screen.getByRole("button", { name: "Apply mock fixer result: Recheck the tip after a fixer commit" }));
    const merge = screen.getByRole("button", { name: "Merge reviewed branch" }) as HTMLButtonElement;
    expect(merge.disabled).toBe(false);
    await fireEvent.click(merge);

    expect(await screen.findByText("MERGE RECORD")).toBeTruthy();
    expect(screen.getByText(/2 findings · 2 fixed · 0 wontfix/)).toBeTruthy();
  });

  it("does not expose Diff as a per-worker action", async () => {
    render(ControlRoom);
    await fireEvent.click(screen.getByRole("button", { name: /Open Theo worker thread/ }));
    expect(screen.queryByRole("button", { name: "Diff" })).toBeNull();
  });
});
