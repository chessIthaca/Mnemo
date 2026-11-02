import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { seedBoardPreview, type BoardTransport } from "../lib/boardEngine";
import type { BoardPlan } from "../lib/boardState";
import { app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import PlanTab from "./PlanTab.svelte";
import WorkerRow from "./WorkerRow.svelte";

const open: BoardPlan = {
  id: "plan-open-0001",
  title: "Open plan",
  goal: "Make it work",
  kind: "feature",
  status: "open",
  closed_at_ms: null,
  report_path: null,
  steps: [
    { number: 1, header: "Engine", text: "Build the engine", done: true, workers: [] },
    {
      number: 2,
      header: null,
      text: "Wire the UI",
      done: false,
      workers: [{ id: "ana", name: "Ana", status: "working" }],
    },
  ],
};

const closed: BoardPlan = {
  id: "plan-done-0002",
  title: "Closed plan",
  goal: "Was done",
  kind: "bug_fixing",
  status: "closed",
  closed_at_ms: 5,
  report_path: ".coding/reviews/closed.md",
  steps: [{ number: 1, header: null, text: "Fix", done: true, workers: [] }],
};

describe("PlanTab", () => {
  beforeEach(() => {
    const state = fixtureState();
    state.board = [open, closed];
    app.set(state);
  });

  it("renders open plans with progress, steps, and worker chips", () => {
    render(PlanTab);
    expect(screen.getByRole("heading", { name: "Open plan" })).toBeTruthy();
    expect(screen.getByText("feature")).toBeTruthy();
    expect(screen.getByText("1/2")).toBeTruthy();
    expect(screen.getByText("Engine")).toBeTruthy();
    expect(screen.getByText("Wire the UI")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Open Ana worker thread" }).textContent).toContain("Ana");
  });

  it("keeps closed plans in a collapsed group with their report path", () => {
    const { container } = render(PlanTab);
    const group = container.querySelector("details.closed") as HTMLDetailsElement;
    expect(group.open).toBe(false);
    expect(group.textContent).toContain("Closed plan");
    expect(group.textContent).toContain(".coding/reviews/closed.md");
  });

  it("opens the worker thread from a chip", async () => {
    render(PlanTab);
    await fireEvent.click(screen.getByRole("button", { name: "Open Ana worker thread" }));
    expect(get(app).route).toBe("team");
    expect(get(app).selectedWorkerId).toBe("ana");
  });

  it("clamps a long goal behind Show more and expands it on demand", async () => {
    // jsdom does no layout: report every clamped block as taller than its clamp.
    const height = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", { configurable: true, get: () => 500 });
    try {
      const { container } = render(PlanTab);
      const toggle = screen.getByRole("button", { name: "Expand Open plan goal" });
      const goal = toggle.previousElementSibling as HTMLElement;
      expect(goal.style.maxHeight).toBe("60px");
      expect(toggle.textContent).toContain("Show more");
      await fireEvent.click(toggle);
      expect(goal.style.maxHeight).toBe("");
      expect(screen.getByRole("button", { name: "Collapse Open plan goal" }).textContent).toContain("Show less");
      expect(container.querySelector(".plan")).toBeTruthy();
    } finally {
      if (height) Object.defineProperty(HTMLElement.prototype, "scrollHeight", height);
      else delete (HTMLElement.prototype as { scrollHeight?: number }).scrollHeight;
    }
  });

  it("offers no toggle for a goal that fits", () => {
    render(PlanTab);
    expect(screen.queryByRole("button", { name: /goal$/ })).toBeNull();
  });

  it("shows an empty state without plans", () => {
    app.update((state) => ({ ...state, board: [] }));
    render(PlanTab);
    expect(screen.getByRole("heading", { name: "No plans" })).toBeTruthy();
  });
});

describe("worker row plan label", () => {
  it("shows the plan title and 1-based step, or the id prefix when unknown", () => {
    const state = fixtureState();
    state.board = [open];
    app.set(state);
    const base = state.workers[0];
    const { unmount } = render(WorkerRow, {
      worker: { ...base, planId: "plan-open-0001", planStep: 1 },
      onSelect: () => {},
    });
    expect(screen.getByText("Open plan · step 2")).toBeTruthy();
    unmount();
    render(WorkerRow, {
      worker: { ...base, planId: "zzzzzzzz-9999", planStep: 0 },
      onSelect: () => {},
    });
    expect(screen.getByText("zzzzzzzz · step 1")).toBeTruthy();
  });
});

/** A plan file for the open card: context, acceptance, steps and decisions. */
const PLAN_DOCUMENT = [
  "# Plan: Open plan",
  "",
  "## Goal",
  "Make it work",
  "",
  "## Context",
  "The card shows only a title and one line per step.",
  "",
  "## Acceptance scenarios",
  "- Details shows the context.",
  "",
  "## Steps",
  "- [x] 1. Build the engine",
  "- [ ] 2. Wire the UI",
  "",
  "## Decisions",
  "- Details is lazy.",
].join("\n");

describe("PlanTab details", () => {
  let changed: (() => void) | null = null;

  /** Plant the board transport the tab reads a plan's document through. */
  function seedDetails(document: (planId: string) => Promise<string>): BoardTransport {
    const transport: BoardTransport = {
      snapshot: vi.fn(async () => ({ plans: [open, closed] })),
      document: vi.fn(document),
      onChanged: vi.fn(async (handler: () => void) => {
        changed = handler;
        return () => {};
      }),
    };
    seedBoardPreview(transport);
    return transport;
  }

  const openCard = () => screen.getByRole("article", { name: "Open plan" });

  beforeEach(() => {
    changed = null;
    const state = fixtureState();
    state.board = [open, closed];
    app.set(state);
  });

  afterEach(() => seedBoardPreview(null));

  it("keeps the card closed by default and expands the document on the first open", async () => {
    const transport = seedDetails(async () => PLAN_DOCUMENT);
    render(PlanTab);
    const card = openCard();
    const toggle = within(card).getByRole("button", { name: "Details" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(card.querySelector(".plan-details")).toBeNull();

    await fireEvent.click(toggle);

    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const details = within(card).getByRole("region", { name: "Open plan details" });
    await waitFor(() => expect(within(details).getByRole("heading", { name: "Context" })).toBeTruthy());
    expect(within(details).getByRole("heading", { name: "Acceptance scenarios" })).toBeTruthy();
    expect(within(details).getByRole("heading", { name: "Decisions" })).toBeTruthy();
    expect(within(details).getByText("The card shows only a title and one line per step.")).toBeTruthy();
    expect(within(details).getByText("Details shows the context.")).toBeTruthy();
    // The title and goal the card already shows are not repeated.
    expect(details.textContent).not.toContain("## Goal");
    expect(details.textContent).not.toContain("Make it work");
    expect(transport.document).toHaveBeenCalledTimes(1);
    expect(transport.document).toHaveBeenCalledWith("plan-open-0001");
  });

  it("fetches once per card: closing and reopening keeps the loaded document", async () => {
    const transport = seedDetails(async () => PLAN_DOCUMENT);
    render(PlanTab);
    const card = openCard();
    const toggle = within(card).getByRole("button", { name: "Details" });

    await fireEvent.click(toggle);
    await waitFor(() => expect(within(card).getByRole("region", { name: "Open plan details" })).toBeTruthy());
    await fireEvent.click(toggle);
    expect(card.querySelector(".plan-details")).toBeNull();
    await fireEvent.click(toggle);

    await waitFor(() => expect(card.querySelector(".plan-details")?.textContent).toContain("Details is lazy."));
    expect(transport.document).toHaveBeenCalledTimes(1);
  });

  it("re-reads an open card when the board changes", async () => {
    const transport = seedDetails(async () => PLAN_DOCUMENT);
    render(PlanTab);
    const card = openCard();
    await fireEvent.click(within(card).getByRole("button", { name: "Details" }));
    await waitFor(() => expect(transport.document).toHaveBeenCalledTimes(1));

    changed?.();

    await waitFor(() => expect(transport.document).toHaveBeenCalledTimes(2));
  });

  it("shows an error line when the document cannot be read", async () => {
    seedDetails(async () => {
      throw "could not read plan plan-open-0001";
    });
    render(PlanTab);
    const card = openCard();

    await fireEvent.click(within(card).getByRole("button", { name: "Details" }));

    await waitFor(() => expect(within(card).getByText("could not read plan plan-open-0001")).toBeTruthy());
    expect(card.querySelector(".details-error")).toBeTruthy();
  });

  it("explains that details need the desktop app when there is no host", async () => {
    render(PlanTab);
    const card = openCard();

    await fireEvent.click(within(card).getByRole("button", { name: "Details" }));

    expect(within(card).getByText("Plan details need the desktop app.")).toBeTruthy();
  });
});
