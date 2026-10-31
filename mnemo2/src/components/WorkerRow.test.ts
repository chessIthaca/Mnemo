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

  it("still opens the worker when the row is clicked", async () => {
    const onSelect = vi.fn();
    render(WorkerRow, { worker: worker(), onSelect });
    await fireEvent.click(screen.getByRole("button", { name: "Open Ana worker thread" }));
    expect(onSelect).toHaveBeenCalledWith("ana");
  });
});