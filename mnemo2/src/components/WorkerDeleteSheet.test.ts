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
    ...overrides,
  };
}

function open(previews: WorkerDeletionPreview[]) {
  app.update((state) => ({ ...state, workerDeletion: { previews, busy: false } }));
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
