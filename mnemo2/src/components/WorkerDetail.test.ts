import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { app } from "../lib/store";
import type { Worker } from "../lib/types";
import { createAuditWorker, createWorkers, fixtureState } from "../test/fixtures";
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

  it("keeps the toggle in the same control row while the brief expands", async () => {
    render(WorkerDetail, { worker: worker(), measureBriefOverflow: () => true });
    const toggle = await screen.findByRole("button", { name: "Show more brief" });
    const brief = document.getElementById(toggle.getAttribute("aria-controls")!);
    const controlRow = toggle.parentElement;

    expect(controlRow?.classList.contains("brief-controls")).toBe(true);
    expect(controlRow?.nextElementSibling).toBe(brief);
    await fireEvent.click(toggle);

    const collapse = screen.getByRole("button", { name: "Show less brief" });
    expect(collapse).toBe(toggle);
    expect(collapse.parentElement).toBe(controlRow);
    expect(controlRow?.nextElementSibling).toBe(brief);
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

  it("opens a repository path named in the brief where it points", async () => {
    const brief = [
      "Goal: fix `.coding/design/unified-tiers.md` and `.coding/plans/abc12345.md`.",
      "Then re-read `src/a.rs:42`.",
      "",
      "```text",
      ".coding/design/fenced-only.md",
      "```",
    ].join("\n");
    const { container } = render(WorkerDetail, {
      worker: { ...worker(), task: brief },
      measureBriefOverflow: () => false,
    });
    const markdown = container.querySelector(".worker-brief .markdown") as HTMLElement;
    const pathButton = (text: string) =>
      [...markdown.querySelectorAll<HTMLButtonElement>("button.md-path")].find(
        (button) => button.textContent === text,
      );

    const design = await waitFor(() => {
      const found = pathButton(".coding/design/unified-tiers.md");
      expect(found).toBeTruthy();
      return found as HTMLButtonElement;
    });
    await fireEvent.click(design);
    expect(get(app).workbenchOpen).toBe(true);
    expect(get(app).workbenchTab).toBe("files");
    expect(get(app).workbenchFile).toEqual({ path: ".coding/design/unified-tiers.md", line: null });

    await fireEvent.click(pathButton(".coding/plans/abc12345.md") as HTMLButtonElement);
    expect(get(app).route).toBe("plans");
    expect(get(app).planDocumentId).toBe("abc12345");

    await fireEvent.click(pathButton("src/a.rs:42") as HTMLButtonElement);
    expect(get(app).workbenchTab).toBe("files");
    expect(get(app).workbenchFile).toEqual({ path: "src/a.rs", line: 42 });

    const fence = markdown.querySelector(".code-block pre") as HTMLElement;
    expect(fence.textContent).toContain(".coding/design/fenced-only.md");
    expect(fence.querySelector("button.md-path")).toBeNull();
  });
});

describe("WorkerDetail compact header", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  it("keeps every worker action available under its accessible name", () => {
    render(WorkerDetail, { worker: createWorkers()[1] });
    for (const name of ["Diff", "Compact", "Pause", "Stop", "Delete Kai"]) {
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

describe("worker thread model-switch note", () => {
  beforeEach(() => app.set(fixtureState()));

  it("renders the muted note in the thread, pending then confirmed", () => {
    const worker: Worker = {
      ...createWorkers()[0],
      messages: [
        { id: "m1", from: "worker", body: "Implementing the parser.", time: "now" },
        {
          id: "note-pending",
          kind: "model_switch",
          model: "test/deep",
          body: "Model changed to deep by you; it takes effect on the next call",
          time: "now",
          applied: false,
        },
        {
          id: "note-applied",
          kind: "model_switch",
          model: "test/fast",
          body: "Now running on fast",
          time: "now",
          applied: true,
        },
      ],
    };

    const { container } = render(WorkerDetail, { worker });

    // The notes render as muted status rows — not as worker messages with authors.
    const notes = [...container.querySelectorAll(".thread-model-note")];
    expect(notes.map((note) => note.getAttribute("role"))).toEqual(["status", "status"]);
    expect(notes.map((note) => note.textContent)).toEqual([
      "Model changed to deep by you; it takes effect on the next call",
      "Now running on fast",
    ]);
  });
});

describe("WorkerDetail finished audit", () => {
  beforeEach(() => app.set(fixtureState()));

  it("hides Resume, Compact, Steer and Stop for a finished audit and points the lead at its report", () => {
    const audit = createAuditWorker();
    render(WorkerDetail, { worker: audit });

    expect(screen.getByText("Audit finished; its report is with the lead.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Steer" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Compact" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Resume" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
    // Delete stays: the lead can still clean the finished audit up.
    expect(screen.getByRole("button", { name: "Delete Ada" })).toBeTruthy();
  });

  it("keeps Resume for an audit that failed before reporting", () => {
    const audit: Worker = { ...createAuditWorker(), status: "failed", reportDelivered: false };
    render(WorkerDetail, { worker: audit });

    expect(screen.getByRole("button", { name: "Resume" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
    expect(screen.queryByText("Audit finished; its report is with the lead.")).toBeNull();
  });
});

describe("WorkerDetail plan tab", () => {
  beforeEach(() => app.set(fixtureState()));

  it("shows the Plan tab for a worker on a plan and renders the plan header", async () => {
    const worker: Worker = { ...createWorkers()[0], planId: "bfa3a622", planStep: 1 };
    render(WorkerDetail, { worker });

    const tabs = screen.getByRole("navigation", { name: "Worker detail sections" });
    await fireEvent.click(within(tabs).getByRole("button", { name: "Plan" }));

    expect(await screen.findByText("Plan bfa3a622 · step 2")).toBeTruthy();
  });

  it("shows no Plan tab for a worker without a plan", () => {
    render(WorkerDetail, { worker: createWorkers()[0] });

    const tabs = screen.getByRole("navigation", { name: "Worker detail sections" });
    expect(within(tabs).queryByRole("button", { name: "Plan" })).toBeNull();
  });
});

describe("WorkerDetail hand-overs stay on the team tab", () => {
  beforeEach(() => app.set({ ...fixtureState(), route: "team" }));

  it("opens Model calls and Diff in the panel beside Mission Control", async () => {
    const target = worker();
    render(WorkerDetail, { worker: target, measureBriefOverflow: () => false });

    await fireEvent.click(screen.getByRole("button", { name: `Model calls ${target.name}` }));
    expect(get(app).route).toBe("team");
    expect(get(app).workbenchOpen).toBe(true);
    expect(get(app).workbenchTab).toBe("trace");
    expect(get(app).traceEntry).toEqual({ agent: target.id });

    await fireEvent.click(screen.getByRole("button", { name: "Diff" }));
    expect(get(app).route).toBe("team");
    expect(get(app).workbenchOpen).toBe(true);
    expect(get(app).workbenchTab).toBe("diff");
  });
});
