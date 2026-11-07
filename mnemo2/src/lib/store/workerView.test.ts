import { describe, expect, it } from "vitest";
import type { TeamWorkerRecord } from "../teamState";
import { buildWorker, elapsedFromMs, timeFromMs } from "./workerView";

/**
 * Characterization tests for the pure worker-view mapping that `store.ts`
 * keeps behind its facade. Every expectation pins the output the store produced
 * before the mapping moved into its own module, so a rewrite of the move cannot
 * quietly change a rendered worker card.
 */
const record = (overrides: Partial<TeamWorkerRecord> = {}): TeamWorkerRecord => ({
  id: "w1",
  name: "Worker w1",
  brief: "do the task",
  model: "test/scripted-a",
  model_id: "scripted-a",
  branch: "wt/agent-w1",
  worktree: "C:/repo/.worktrees/agent-w1",
  status: "working",
  generation: 1,
  transcript: { messages: [{ role: "assistant", content: "working on it" }] },
  pending: null,
  used_tokens: 10,
  max_tokens: 100,
  total_tokens: 10,
  prompt_tokens: 8,
  completion_tokens: 2,
  cached_tokens: 3,
  requests: 2,
  cost_micros: 500_000,
  created_at_ms: 1_787_399_996_000,
  elapsed_ms: 4000,
  turns: 2,
  last_activity_ms: 1_787_400_000_000,
  checks: [{ name: "workspace tests", status: "passed", detail: "ok", tip: "tip-1" }],
  findings: [],
  review: { summary: "reviewed", tip: "tip-1", reviewed_at_ms: 1_787_400_000_000 },
  fixer_finding_id: null,
  merge: null,
  error: null,
  ...overrides,
});

describe("workerView", () => {
  it("formats elapsed time in the coarsest whole unit", () => {
    expect(elapsedFromMs(0)).toBe("—");
    expect(elapsedFromMs(59_000)).toBe("59s");
    expect(elapsedFromMs(61_000)).toBe("1m");
    expect(elapsedFromMs(3_600_000)).toBe("1h");
    expect(elapsedFromMs(172_800_000)).toBe("2d");
  });

  it("reports an unknown clock time for zero and non-finite stamps", () => {
    expect(timeFromMs(0)).toBe("unknown");
    expect(timeFromMs(NaN)).toBe("unknown");
  });

  it("maps finished and cancelled statuses to the ends of the progress bar", () => {
    expect(buildWorker(record({ status: "merged" }), 0, [], "merged", null).progress).toBe(100);
    expect(buildWorker(record({ status: "cancelled" }), 0, [], "cancelled", null).progress).toBe(0);
  });

  it("flags a conflict but not a working worker with no pending question", () => {
    expect(buildWorker(record({ status: "conflict" }), 0, [], "conflict", null).needsYou).toBe(true);
    const working = buildWorker(record({ status: "working", pending: null }), 0, [], "working", null);
    expect(working.needsYou).toBe(false);
  });

  it("formats the worker's elapsed time through elapsedFromMs", () => {
    expect(buildWorker(record({ elapsed_ms: 61_000 }), 0, [], "working", null).elapsed).toBe("1m");
  });
});
