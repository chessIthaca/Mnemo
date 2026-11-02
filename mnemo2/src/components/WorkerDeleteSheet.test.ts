import { fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import type { WorkerDeletionPreview } from "../lib/teamState";
import { fixtureState } from "../test/fixtures";
import WorkerDeleteSheet from "./WorkerDeleteSheet.svelte";

function preview(overrides: Partial<WorkerDeletionPreview> = {}): WorkerDeletionPreview {
  return {
    worker_id: "w1",
    name: "Worker w1",
    branch: "wt/agent-w1",
    live: false,
    checkout: true,
    uncommitted: false,
    unmerged_commits: 0,
    shared_branch: false,
    blocked: null,
    superseded_by: null,
    ...overrides,
  };
}

function open(previews: WorkerDeletionPreview[]) {
  app.update((state) => ({
    ...state,
    workerDeletion: {
      phase: "ready",
      rows: previews.map((preview) => ({
        workerId: preview.worker_id,
        name: preview.name,
        preview,
        error: null,
        deleted: false,
      })),
    },
  }));
}

describe("WorkerDeleteSheet", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("names what a clean delete removes and deletes on confirm", async () => {
    const confirm = vi.spyOn(actions, "confirmDeleteWorkers").mockResolvedValue(undefined);
    open([preview({ live: true })]);
    render(WorkerDeleteSheet);
    expect(screen.getByText("Delete Worker w1?")).toBeTruthy();
    expect(screen.getByText(/stops its running session · removes its checkout · deletes wt\/agent-w1/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(confirm).toHaveBeenCalled();
  });

  it("spells out unmerged and uncommitted work before discarding it", () => {
    open([preview({ unmerged_commits: 3, uncommitted: true })]);
    render(WorkerDeleteSheet);
    expect(screen.getByText("Discards 3 unmerged commits and uncommitted changes.")).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("not on the lead branch");
    expect(screen.getByRole("button", { name: "Delete and discard work" })).toBeTruthy();
  });

  it("names the worker that landed a superseded step instead of warning", async () => {
    const confirm = vi.spyOn(actions, "confirmDeleteWorkers").mockResolvedValue(undefined);
    open([preview({ unmerged_commits: 1, superseded_by: "senderReplay36" })]);
    render(WorkerDeleteSheet);
    expect(screen.getByText(/Its step landed through senderReplay36/)).toBeTruthy();
    expect(screen.getByText(/its own 1 unmerged commit not on the lead branch will be discarded/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("button", { name: "Delete" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(confirm).toHaveBeenCalled();
  });

  it("counts only work no other worker landed in the warning", () => {
    open([
      preview({ worker_id: "w1", unmerged_commits: 1, superseded_by: "senderReplay36" }),
      preview({ worker_id: "w2", name: "Worker w2", unmerged_commits: 2 }),
    ]);
    render(WorkerDeleteSheet);
    expect(screen.getByText(/Its step landed through senderReplay36/)).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("1 worker has work that is not on the lead branch");
    expect(screen.getByRole("button", { name: "Delete and discard work" })).toBeTruthy();
  });

  it("shows each worker as checking until its preview arrives", () => {
    app.update((state) => ({
      ...state,
      workerDeletion: {
        phase: "checking",
        rows: [{ workerId: "w1", name: "Worker w1", preview: null, error: null, deleted: false }],
      },
    }));
    render(WorkerDeleteSheet);
    expect(screen.getByText("Checking workers…")).toBeTruthy();
    expect(screen.getAllByText("Checking…")).not.toHaveLength(0);
    const confirm = screen.getByRole("button", { name: "Checking…" });
    expect(confirm.hasAttribute("disabled")).toBe(true);
  });

  it("keeps a shared branch and lists blocked workers without deleting them", () => {
    open([
      preview({ worker_id: "w1", shared_branch: true, checkout: false }),
      preview({ worker_id: "w2", name: "Worker w2", blocked: "a merge is in progress" }),
    ]);
    render(WorkerDeleteSheet);
    expect(screen.getByText("Delete Worker w1?")).toBeTruthy();
    expect(screen.getByText(/keeps wt\/agent-w1, which another worker still uses/)).toBeTruthy();
    expect(screen.getByText("Kept: a merge is in progress")).toBeTruthy();
  });
});
