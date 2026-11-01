import { fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { app } from "../lib/store";
import type { Worker } from "../lib/types";
import { createWorkers, fixtureState } from "../test/fixtures";
import WorkerRow from "./WorkerRow.svelte";

const brief = [
  "# Plan: Mission control summaries",
  "",
  "- clamp the row with `-webkit-line-clamp`",
  "- keep the **full brief** in the worker detail",
].join("\n");

const worker = (): Worker => ({ ...createWorkers()[0], task: brief });

describe("WorkerRow", () => {
  beforeEach(() => app.set(fixtureState()));

  it("renders the summary, not the raw Markdown, in the three-line clamp", () => {
    const { container } = render(WorkerRow, { worker: worker(), onSelect: vi.fn() });
    // The clamp lives on .worker-task; it must hold the plain summary.
    const task = container.querySelector<HTMLElement>(".worker-task");
    expect(task?.textContent).toBe(
      "Plan: Mission control summaries clamp the row with -webkit-line-clamp keep the full brief in the worker detail",
    );
    expect(task?.getAttribute("title")).toBe(task?.textContent);
    expect(container.textContent).not.toContain("**");
    expect(container.textContent).not.toContain("`");
  });

  it("uses small muted task text and a visible progress track", () => {
    render(WorkerRow, { worker: worker(), onSelect: vi.fn() });

    const summary = document.querySelector<HTMLElement>(".worker-task");
    const progress = document.querySelector<HTMLElement>(".worker-progress");
    const bar = progress?.querySelector<HTMLElement>("span");
    if (!summary || !progress || !bar) throw new Error("Worker summary or progress bar missing");

    const summaryStyle = getComputedStyle(summary);
    expect(summaryStyle.fontFamily).toBe("var(--font-ui)");
    expect(summaryStyle.fontSize).toBe("var(--text-meta)");
    expect(summaryStyle.color).toBe("var(--muted)");
    expect(summaryStyle.webkitLineClamp).toBe("3");

    const progressStyle = getComputedStyle(progress);
    expect(progressStyle.display).toBe("block");
    expect(progressStyle.height).toBe("2px");
    expect(progressStyle.marginTop).toBe("7px");
    expect(progressStyle.overflow).toBe("hidden");
    expect(getComputedStyle(bar).display).toBe("block");
    expect(getComputedStyle(bar).height).toBe("100%");
  });
  it("still opens the worker when the row is clicked", async () => {
    const onSelect = vi.fn();
    render(WorkerRow, { worker: worker(), onSelect });
    await fireEvent.click(screen.getByRole("button", { name: "Open Ana worker thread" }));
    expect(onSelect).toHaveBeenCalledWith("ana");
  });

  it("shows the live phase and a ticking timer while the worker runs", () => {
    const running = {
      ...worker(),
      status: "working",
      createdAtMs: Date.now() - 18 * 60_000,
      activity: {
        phase: "tool",
        toolName: "shell",
        turnStartedAtMs: Date.now() - 72_000,
        phaseSinceMs: Date.now() - 5_000,
      },
    } as Worker;
    const { container } = render(WorkerRow, { worker: running, onSelect: vi.fn() });

    const live = container.querySelector<HTMLElement>(".worker-live .thinking-status");
    expect(live?.textContent).toContain("Running shell…");
    // The timer ticks since the worker was created, not the static elapsed text.
    expect(live?.textContent).toContain("18:");
    expect(live?.classList.contains("meta")).toBe(true);
    expect(getComputedStyle(live!).fontSize).toBe("var(--text-meta)");
    // The static cost-and-elapsed line is gone while the worker runs.
    expect(container.querySelector(".worker-meta small")).toBeNull();
  });

  it("keeps the dot solid when the worker waits on the human", () => {
    const waiting = {
      ...worker(),
      status: "needs_you",
      needsYou: true,
      createdAtMs: Date.now() - 5 * 60_000,
      activity: {
        phase: "waiting",
        toolName: null,
        turnStartedAtMs: Date.now() - 30_000,
        phaseSinceMs: Date.now() - 1_000,
      },
    } as Worker;
    const { container } = render(WorkerRow, { worker: waiting, onSelect: vi.fn() });

    const live = container.querySelector<HTMLElement>(".worker-live .thinking-status");
    expect(live?.textContent).toContain("Waiting on you");
    expect(live?.classList.contains("still")).toBe(true);
  });

  it("keeps the finished worker's static cost and elapsed line", () => {
    const { container } = render(WorkerRow, { worker: worker(), onSelect: vi.fn() });

    const line = container.querySelector<HTMLElement>(".worker-meta small");
    expect(line?.textContent).toContain("$1.84 · 26m");
    expect(container.querySelector(".worker-live")).toBeNull();
  });
});