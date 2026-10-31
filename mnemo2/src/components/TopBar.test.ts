import { fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import TopBar from "./TopBar.svelte";

const costButton = () => screen.getByRole("button", { name: /Project cost/i });
const usageTip = () => document.getElementById("project-usage-detail") as HTMLElement;
const chipText = () => (costButton().textContent ?? "").replace(/\s+/g, " ").trim();

describe("TopBar project usage", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  it("shows the durable USD cost instead of a token fallback", () => {
    const state = fixtureState();
    // Every worker lost its per-worker price, and the project aggregate is
    // the only source of truth — the old header counted tokens here.
    app.set({
      ...state,
      workers: state.workers.map((worker) => ({ ...worker, cost: null })),
      projectUsage: {
        inputTokens: 0,
        cachedTokens: 0,
        outputTokens: 0,
        costMicros: 0,
        unpricedModels: [],
      },
    });
    render(TopBar);

    expect(chipText()).toBe("$0.00");
    expect(chipText()).not.toMatch(/token/i);
  });

  it("renders the persisted cost from the project usage aggregate", () => {
    render(TopBar);

    expect(chipText()).toBe("$4.32");
    expect(costButton().getAttribute("aria-label")).toMatch(/Project cost \$4\.32/);
  });

  it("exposes cached, uncached input, output, and total tokens on hover and focus", async () => {
    render(TopBar);
    const button = costButton();
    const tip = usageTip();

    expect(button.getAttribute("aria-describedby")).toBe("project-usage-detail");
    expect(tip.getAttribute("role")).toBe("tooltip");
    expect(tip.classList.contains("open")).toBe(false);

    await fireEvent.mouseEnter(button);
    expect(tip.classList.contains("open")).toBe(true);
    await fireEvent.mouseLeave(button);
    expect(tip.classList.contains("open")).toBe(false);

    await fireEvent.focus(button);
    expect(tip.classList.contains("open")).toBe(true);

    expect(tip.textContent).toContain("Cached input");
    expect(tip.textContent).toContain("212,400 tokens");
    expect(tip.textContent).toContain("Uncached input");
    expect(tip.textContent).toContain("486,200 tokens");
    expect(tip.textContent).toContain("Output");
    expect(tip.textContent).toContain("64,800 tokens");
    expect(tip.textContent).toContain("Total");
    expect(tip.textContent).toContain("763,400 tokens");

    await fireEvent.blur(button);
    expect(tip.classList.contains("open")).toBe(false);
  });

  it("states plainly when a project has recorded no usage yet", () => {
    const state = fixtureState();
    app.set({
      ...state,
      projectUsage: {
        inputTokens: 0,
        cachedTokens: 0,
        outputTokens: 0,
        costMicros: 0,
        unpricedModels: [],
      },
    });
    render(TopBar);

    expect(chipText()).toBe("$0.00");
    expect(usageTip().textContent).toContain("No usage recorded for this project yet.");
    expect(usageTip().textContent).toContain("0 tokens");
  });

  it("marks a partial estimate without reverting to tokens when a model is unpriced", () => {
    const state = fixtureState();
    app.set({
      ...state,
      projectUsage: {
        ...state.projectUsage,
        unpricedModels: ["mystery-model", "kimi-k3.1-pro"],
      },
    });
    render(TopBar);

    expect(chipText()).toBe("$4.32+");
    expect(costButton().getAttribute("aria-label")).toMatch(/partial estimate/);
    expect(usageTip().textContent).toContain("No price for: mystery-model, kimi-k3.1-pro");
    expect(usageTip().textContent).toContain("The cost covers priced usage only.");
  });
});

describe("TopBar external lead badge", () => {
  const badge = () => screen.queryByText("Lead: Claude Code (external)");

  beforeEach(() => {
    app.set(fixtureState());
  });

  it("is hidden while the built-in lead leads", () => {
    render(TopBar);
    expect(badge()).toBeNull();
  });

  it("shows while Claude Code is attached, with an explaining tooltip, and hides on detach", async () => {
    render(TopBar);

    app.update((state) => ({
      ...state,
      externalLead: { attached: true, root: "C:/Mnemo", sinceMs: 1_000 },
    }));
    const shown = await screen.findByText("Lead: Claude Code (external)");
    expect(shown.closest("[title]")?.getAttribute("title")).toBe(
      "Claude Code is leading this project through the mnemo plugin. Mnemo2 sends worker notices to it instead of the built-in lead.",
    );

    app.update((state) => ({
      ...state,
      externalLead: { attached: false, root: null, sinceMs: null },
    }));
    await vi.waitFor(() => expect(badge()).toBeNull());
  });
});
