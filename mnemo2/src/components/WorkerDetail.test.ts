import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { app } from "../lib/store";
import type { Worker } from "../lib/types";
import { createWorkers, fixtureState } from "../test/fixtures";
import WorkerDetail from "./WorkerDetail.svelte";

const longBrief = [
  "# Plan: Short worker summaries",
  "",
  "Goal: Mission control shows each worker's task as **at most three lines**.",
  "",
  "- add a briefSummary helper in mnemo2/src/lib/brief.ts",
  "- clamp the row summary with CSS",
  "- keep the full brief one click away in the detail",
  "",
  "Verify with npm test and npm run check.",
].join("\n");

const worker = (): Worker => ({ ...createWorkers()[0], task: longBrief });

describe("WorkerDetail brief", () => {
  beforeEach(() => app.set(fixtureState()));

  it("renders the full brief as Markdown inside the clamped brief, not as a heading", () => {
    const { container } = render(WorkerDetail, { worker: worker(), measureBriefOverflow: () => true });
    const brief = container.querySelector(".worker-brief .brief-content .markdown");
    expect(brief).toBeTruthy();
    // The whole Markdown brief is rendered, markers become real elements.
    expect(brief?.querySelector("h1")?.textContent).toBe("Plan: Short worker summaries");
    expect(brief?.querySelector("strong")?.textContent).toBe("at most three lines");
    expect([...(brief?.querySelectorAll("li") ?? [])].map((li) => li.textContent)).toContain(
      "add a briefSummary helper in mnemo2/src/lib/brief.ts",
    );
    // No heading element in the detail header carries the raw brief.
    expect(container.querySelector(".worker-heading h3")).toBeNull();
    expect(container.querySelector("h3")?.textContent ?? "").not.toBe(longBrief);
  });

  it("expands and collapses in place, and Brief expands the same brief", async () => {
    const measure = vi.fn(() => true);
    render(WorkerDetail, { worker: worker(), measureBriefOverflow: measure });
    await waitFor(() => expect(measure).toHaveBeenCalled());
    const toggle = await screen.findByRole("button", { name: "Show more brief" });
    const brief = document.getElementById(toggle.getAttribute("aria-controls")!);
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(brief?.classList.contains("clamped")).toBe(true);
    await fireEvent.click(toggle);
    expect(screen.getByRole("button", { name: "Show less brief" }).getAttribute("aria-expanded")).toBe("true");
    expect(brief?.classList.contains("clamped")).toBe(false);
    await fireEvent.click(screen.getByRole("button", { name: "Show less brief" }));
    expect(screen.getByRole("button", { name: "Show more brief" }).getAttribute("aria-expanded")).toBe("false");
    expect(brief?.classList.contains("clamped")).toBe(true);

    const workflow = screen.getByRole("list", { name: /Task workflow:/ });
    await fireEvent.click(within(workflow).getByRole("button", { name: "Workflow: Brief" }));
    expect(screen.getByRole("button", { name: "Show less brief" })).toBeTruthy();
  });

  it("keeps the expanded brief bounded and scrollable", async () => {
    render(WorkerDetail, { worker: worker(), measureBriefOverflow: () => true });
    const toggle = await screen.findByRole("button", { name: "Show more brief" });
    const brief = document.getElementById(toggle.getAttribute("aria-controls")!) as HTMLElement;

    await fireEvent.click(toggle);

    expect(brief.style.maxHeight).toBe("40vh");
    expect(brief.style.overflowY).toBe("auto");
  });

  it("offers no toggle when the brief fits", async () => {
    const measure = vi.fn(() => false);
    render(WorkerDetail, {
      worker: { ...worker(), task: "A short brief." },
      measureBriefOverflow: measure,
    });
    await waitFor(() => expect(measure).toHaveBeenCalled());
    expect(screen.queryByRole("button", { name: /Show (more|less) brief/ })).toBeNull();
  });
});

describe("WorkerDetail compact header", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  it("keeps every worker action available under its accessible name", () => {
    render(WorkerDetail, { worker: createWorkers()[1] });
    for (const name of ["Diff", "Steer", "Compact", "Pause", "Stop", "Delete Kai"]) {
      expect(screen.getByRole("button", { name })).toBeTruthy();
    }
  });

  it("navigates with the workflow steps", async () => {
    render(WorkerDetail, { worker: createWorkers()[0] });
    const workflow = screen.getByRole("list", { name: /Task workflow:/ });
    const tabs = screen.getByRole("navigation", { name: "Worker detail sections" });

    await fireEvent.click(within(workflow).getByRole("button", { name: "Workflow: Review" }));
    expect(within(tabs).getByRole("button", { name: /^Review/ }).getAttribute("aria-pressed")).toBe("true");

    await fireEvent.click(within(workflow).getByRole("button", { name: "Workflow: Implement" }));
    expect(within(tabs).getByRole("button", { name: /^Thread/ }).getAttribute("aria-pressed")).toBe("true");
    expect(within(tabs).getByRole("button", { name: /^Review/ }).getAttribute("aria-pressed")).toBe("false");

    await fireEvent.click(within(workflow).getByRole("button", { name: "Workflow: Verify" }));
    expect(within(tabs).getByRole("button", { name: /^Review/ }).getAttribute("aria-pressed")).toBe("true");
  });
});

describe("WorkerDetail received messages", () => {
  beforeEach(() => app.set(fixtureState()));

  it("shows the inbox ledger above the thread when the worker received messages", () => {
    const { container } = render(WorkerDetail, {
      worker: {
        ...createWorkers()[0],
        inbox: [
          { sender: "user", kind: "steer", text: "Keep going.", atMs: 1_787_400_000_000 },
        ],
      },
    });

    const inbox = container.querySelector(".inbox-panel");
    expect(inbox).toBeTruthy();
    expect(inbox?.textContent).toContain("You");
    // The ledger sits above the conversation it fed.
    const body = [...(container.querySelector(".detail-body")?.children ?? [])];
    expect(
      body.findIndex((node) => node.classList.contains("inbox-panel")),
    ).toBeLessThan(body.findIndex((node) => node.classList.contains("thread-panel")));
  });

  it("keeps the section out of the thread tab when nothing was received", () => {
    const { container } = render(WorkerDetail, { worker: createWorkers()[0] });

    expect(container.querySelector(".inbox-panel")).toBeNull();
  });
});
