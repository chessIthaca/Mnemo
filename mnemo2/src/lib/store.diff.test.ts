import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "./store";
import { fixtureState } from "../test/fixtures";
import type { DiffSnapshot } from "./types";

const { transport } = vi.hoisted(() => ({
  transport: { diff: vi.fn(), screenshot: vi.fn() },
}));
vi.mock("./workbenchEngine", () => ({
  createWorkbenchTransport: () => transport,
}));

function diff(workerId: string, marker: string): DiffSnapshot {
  return {
    worker_id: workerId,
    branch: `wt/agent-${workerId}`,
    target: "wt/lead",
    base: "a".repeat(40),
    tip: marker.repeat(40),
    files: [{ path: `${marker}.rs`, additions: 1, deletions: 0, lines: [] }],
    total_files: 1,
    total_additions: 1,
    total_deletions: 0,
    truncated: false,
  };
}

describe("real workbench diffs", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    const state = fixtureState();
    const [first, second] = state.workers;
    state.projectId = "project";
    state.leadRuntime.mode = "native";
    state.workers = [{ ...first, id: "one" }, { ...second, id: "two" }];
    state.diffWorkerId = "one";
    app.set(state);
  });

  it("ignores an older response after the selected worker changes", async () => {
    const resolvers = new Map<string, (value: DiffSnapshot) => void>();
    transport.diff.mockImplementation((workerId: string) => new Promise<DiffSnapshot>((resolve) => {
      resolvers.set(workerId, resolve);
    }));
    actions.selectDiffWorker("one");
    actions.selectDiffWorker("two");
    resolvers.get("two")?.(diff("two", "b"));
    await vi.waitFor(() => expect(get(app).diff?.worker_id).toBe("two"));
    resolvers.get("one")?.(diff("one", "a"));
    await Promise.resolve();
    expect(get(app).diff?.worker_id).toBe("two");
    expect(get(app).diffFiles[0].path).toBe("b.rs");
    expect(get(app).workers.find((worker) => worker.id === "two")?.files).toEqual(["b.rs"]);
  });
});
