import { fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { app } from "../lib/store";
import type { Worker } from "../lib/types";
import { createWorkers, fixtureState } from "../test/fixtures";
import ActivityStatus from "./ActivityStatus.svelte";
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

  it("puts every row's status in one fixed-width column", () => {
    const { container } = render(WorkerRow, { worker: worker(), onSelect: vi.fn() });

    const meta = container.querySelector<HTMLElement>(".worker-meta");
    if (!meta) throw new Error("status column missing");
    const column = getComputedStyle(meta);
    // A fixed basis: the column neither grows nor shrinks with its text, so the pill and
    // the line under it start at the same x on every row.
    expect(column.display).toBe("flex");
    expect(column.flexDirection).toBe("column");
    expect(column.alignItems).toBe("flex-start");
    expect(column.flexGrow).toBe("0");
    expect(column.flexShrink).toBe("0");
    expect(column.flexBasis).toBe("152px");
    expect(column.minWidth).toBe("0px");

    // The pill keeps its natural width; the line under it fills the column and truncates.
    const pill = meta.querySelector<HTMLElement>(".status-label");
    if (!pill) throw new Error("status pill missing");
    expect(getComputedStyle(pill).display).toBe("inline-flex");

    const line = meta.querySelector<HTMLElement>("small");
    if (!line) throw new Error("cost and elapsed line missing");
    const lineStyle = getComputedStyle(line);
    expect(lineStyle.width).toBe("100%");
    expect(lineStyle.minWidth).toBe("0px");
    expect(lineStyle.overflow).toBe("hidden");
    expect(lineStyle.textOverflow).toBe("ellipsis");
    expect(lineStyle.whiteSpace).toBe("nowrap");
  });

  it("truncates a long live label in the column and never the timer", () => {
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
    if (!live) throw new Error("live status row missing");
    // The row opts the shared status row into truncation; the lead chat and thread do not.
    expect(live.classList.contains("truncate")).toBe(true);
    expect(getComputedStyle(live).minWidth).toBe("0px");

    const label = live.querySelector<HTMLElement>(".thinking-label");
    if (!label) throw new Error("live label missing");
    const labelStyle = getComputedStyle(label);
    expect(labelStyle.minWidth).toBe("0px");
    expect(labelStyle.overflow).toBe("hidden");
    expect(labelStyle.textOverflow).toBe("ellipsis");
    expect(labelStyle.whiteSpace).toBe("nowrap");

    // The timer is the part that must stay whole: `flex: none` keeps it from shrinking.
    const timer = live.querySelector<HTMLElement>(".elapsed-time");
    if (!timer) throw new Error("live timer missing");
    expect(getComputedStyle(timer).flexShrink).toBe("0");
    expect(getComputedStyle(timer).whiteSpace).toBe("nowrap");
  });

  it("leaves the shared status row untruncated where it is not a fixed column", () => {
    const { container } = render(ActivityStatus, {
      label: "Thinking…",
      startedAtMs: Date.now() - 5_000,
    });

    const status = container.querySelector<HTMLElement>(".thinking-status");
    if (!status) throw new Error("shared status row missing");
    // Opt-in only: without the row's `truncate` the label keeps its natural width.
    expect(status.classList.contains("truncate")).toBe(false);
    const label = status.querySelector<HTMLElement>(".thinking-label");
    if (!label) throw new Error("shared status label missing");
    expect(getComputedStyle(label).textOverflow).not.toBe("ellipsis");
    expect(getComputedStyle(label).minWidth).not.toBe("0px");
  });
  it("shows the dispatch routing chip and the live nudge chip", () => {
    const routed: Worker = {
      ...worker(),
      tier: "routine",
      routing: {
        laya_tier: "routine",
        confidence: 0.62,
        cleared: true,
        lead_tier: null,
        routed_by: "laya",
        ran_tier: "routine",
        nudged: "high",
      },
    };
    const { container } = render(WorkerRow, { worker: routed, onSelect: vi.fn() });
    expect(screen.getByText("Laya: routine 0.62")).toBeTruthy();
    expect(screen.getByText("Laya suggests high — stuck")).toBeTruthy();
    expect(container.querySelectorAll(".route-chip")).toHaveLength(2);
    // The chips sit on their own line under the name; the name line keeps the model.
    expect(container.querySelector(".worker-name .route-chip")).toBeNull();
    expect(container.querySelector(".worker-route .route-chip")).not.toBeNull();
    expect(container.querySelector(".worker-name .model-badge")?.textContent).toContain("kimi-k3.1-pro");
  });

  it("clears the nudge chip once the worker finished, keeping the routing chip", () => {
    const routed: Worker = {
      ...worker(),
      status: "merged",
      tier: "routine",
      routing: {
        laya_tier: "routine",
        confidence: 0.62,
        cleared: true,
        lead_tier: null,
        routed_by: "laya",
        ran_tier: "routine",
        nudged: "high",
      },
    };
    const { container } = render(WorkerRow, { worker: routed, onSelect: vi.fn() });
    expect(screen.queryByText(/Laya suggests/)).toBeNull();
    expect(screen.getByText("Laya: routine 0.62")).toBeTruthy();
    expect(container.querySelectorAll(".route-chip")).toHaveLength(1);
    // The routing chip moved off the name line; the model stayed on it.
    expect(container.querySelector(".worker-name .route-chip")).toBeNull();
    expect(container.querySelector(".worker-route .route-chip")).not.toBeNull();
    expect(container.querySelector(".worker-name .model-badge")?.textContent).toContain("kimi-k3.1-pro");
  });

  it("renders no route line when the dispatch recorded no routing", () => {
    const unrouted: Worker = { ...worker(), routing: null };
    const { container } = render(WorkerRow, { worker: unrouted, onSelect: vi.fn() });

    // No decision and no nudge: there is no route line at all…
    expect(container.querySelector(".worker-route")).toBeNull();
    // …and the model still sits on the name line.
    expect(container.querySelector(".worker-name .model-badge")?.textContent).toContain("kimi-k3.1-pro");
  });
});
