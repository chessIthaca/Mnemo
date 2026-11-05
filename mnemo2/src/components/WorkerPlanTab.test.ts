import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { seedBoardPreview, type BoardTransport } from "../lib/boardEngine";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import WorkerPlanTab from "./WorkerPlanTab.svelte";

/** A plan document with a goal, spec changes and steps — the whole plan. */
const PLAN_MD = [
  "# Plan: Tier ladder",
  "",
  "## Goal",
  "Show the tier ladder.",
  "",
  "## Spec changes",
  "### tier-ladder",
  "#### ADDED Requirements",
  "##### Requirement: Ladder renders",
  "The board SHALL show the ladder.",
  "",
  "## Steps",
  "- [ ] 1. Build the ladder rows",
  "- [ ] 2. Wire the plan tab",
].join("\n");

/** Plant the board transport the tab reads the plan document through. */
function seedPlan(document: (planId: string) => Promise<string> = async () => PLAN_MD): void {
  const transport: BoardTransport = {
    snapshot: async () => ({ plans: [] }),
    document,
    onChanged: async () => () => {},
  };
  seedBoardPreview(transport);
}

describe("WorkerPlanTab", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  afterEach(() => {
    seedBoardPreview(null);
    vi.restoreAllMocks();
  });

  it("shows the whole plan with the worker's step in the header", async () => {
    seedPlan();
    render(WorkerPlanTab, { planId: "bfa3a622", planStep: 1 });

    // The whole document renders: the goal, the spec changes and the steps.
    expect(await screen.findByText("Show the tier ladder.")).toBeTruthy();
    expect(screen.getByText("Plan bfa3a622 · step 2")).toBeTruthy();
    expect(screen.getByText("Spec changes")).toBeTruthy();
    expect(screen.getByText(/Wire the plan tab/)).toBeTruthy();
    // Every capability the spec changes touch folds its current spec below.
    expect(await screen.findByText("Current spec: tier-ladder")).toBeTruthy();
  });

  it("opens the plan in the Plans route from the header button", async () => {
    seedPlan();
    const open = vi.spyOn(actions, "openPlanDocument").mockImplementation(() => {});
    render(WorkerPlanTab, { planId: "bfa3a622", planStep: 1 });

    await screen.findByText("Plan bfa3a622 · step 2");
    await fireEvent.click(screen.getByRole("button", { name: "Open in Plans" }));

    expect(open).toHaveBeenCalledWith("bfa3a622");
  });

  it("says the specs need the desktop app when their fold opens in a plain browser", async () => {
    seedPlan();
    render(WorkerPlanTab, { planId: "bfa3a622", planStep: null });

    expect(await screen.findByText("Plan bfa3a622")).toBeTruthy();
    // The capabilities come from the plan's own document, so await it.
    const fold = (await screen.findByText("Current spec: tier-ladder")).closest("details") as HTMLDetailsElement;
    fold.open = true; // jsdom does not toggle details on summary clicks.
    fold.dispatchEvent(new Event("toggle"));

    await waitFor(() => expect(screen.getByText("Specs need the desktop app.")).toBeTruthy());
  });

  it("needs the desktop app when no board transport exists", async () => {
    render(WorkerPlanTab, { planId: "bfa3a622", planStep: 0 });

    expect(await screen.findByText("Plan details need the desktop app.")).toBeTruthy();
    expect(screen.getByText("Plan bfa3a622 · step 1")).toBeTruthy();
  });

  it("re-reads the plan when the board changes", async () => {
    let document = PLAN_MD;
    const boardChanged: (() => void)[] = [];
    seedBoardPreview({
      snapshot: async () => ({ plans: [] }),
      document: async () => document,
      onChanged: async (handler) => {
        boardChanged.push(handler);
        return () => {};
      },
    });
    render(WorkerPlanTab, { planId: "bfa3a622", planStep: 0 });

    expect(await screen.findByText("Show the tier ladder.")).toBeTruthy();
    document = PLAN_MD.replace("Show the tier ladder.", "Show the edited ladder.");
    boardChanged.forEach((handler) => handler());

    expect(await screen.findByText("Show the edited ladder.")).toBeTruthy();
    expect(screen.queryByText("Show the tier ladder.")).toBeNull();
  });
});
