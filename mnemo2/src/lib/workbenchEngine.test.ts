import { describe, expect, it } from "vitest";
import { createTauriWorkbenchTransport } from "./workbenchEngine";
import type { DiffSnapshot } from "./types";

const snapshot: DiffSnapshot = {
  worker_id: "worker-1",
  branch: "wt/agent-worker-1",
  target: "wt/lead",
  base: "a".repeat(40),
  tip: "b".repeat(40),
  files: [],
  total_files: 0,
  total_additions: 0,
  total_deletions: 0,
  truncated: false,
};

describe("workbench native transport", () => {
  it("reads diffs and screenshots through their typed commands", async () => {
    const invocations: [string, unknown][] = [];
    const invoke = async <T,>(command: string, args?: unknown): Promise<T> => {
      invocations.push([command, args]);
      return (command === "workbench_diff" ? snapshot : "data:image/png;base64,real") as T;
    };
    const transport = createTauriWorkbenchTransport({ invoke });
    await expect(transport.diff("worker-1")).resolves.toBe(snapshot);
    await transport.screenshot("worker-1", ".coding/browser/screenshots/page.png");
    expect(invocations).toEqual([
      ["workbench_diff", { workerId: "worker-1" }],
      ["workbench_screenshot", {
        workerId: "worker-1",
        path: ".coding/browser/screenshots/page.png",
      }],
    ]);
  });
});
