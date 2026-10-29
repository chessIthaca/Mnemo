import { describe, expect, it } from "vitest";
import {
  applyTeamEvent,
  applyTeamSnapshot,
  emptyProjectUsageView,
  initialTeamState,
  projectUsageFrom,
  type TeamEvent,
  type TeamWorkerRecord,
} from "./teamState";

const worker: TeamWorkerRecord = {
  id: "w1",
  name: "Worker w1",
  brief: "do the task",
  model: "test/scripted-a",
  model_id: "scripted-a",
  branch: "wt/agent-w1",
  worktree: "C:/repo/.worktrees/agent-w1",
  status: "working",
  generation: 1,
  transcript: {
    messages: [{ role: "assistant", content: "working on it" }],
  },
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
  findings: [{
    id: "F-1",
    severity: "low",
    title: "Example",
    detail: "Detail",
    file: "src/lib.rs",
    line: 1,
    status: "open",
    fix_commit: null,
    evidence: null,
    justification: null,
  }],
  review: { summary: "reviewed", tip: "tip-1", reviewed_at_ms: 1_787_400_000_000 },
  fixer_finding_id: null,
  merge: null,
  error: null,
};

const event = (kind: string, extra: Record<string, unknown> = {}, generation = 1): TeamEvent => ({
  worker_id: "w1",
  generation,
  event: { agent: 1, event: { kind, ...extra } },
});

describe("teamState", () => {
  it("restores real workers from a snapshot", () => {
    const state = applyTeamSnapshot({ workers: [worker], slots: 2, active: 1, usage: emptyProjectUsageView() });
    expect(state.workers).toHaveLength(1);
    expect(state.workers[0]).toMatchObject({
      id: "w1",
      text: "working on it",
      status: "working",
      usedTokens: 10,
    });
    expect(state.active).toBe(1);
    expect(state.workers[0].totalTokens).toBe(10);
    expect(state.workers[0].costMicros).toBe(500_000);
    expect(state.workers[0].elapsedMs).toBe(4000);
    expect(state.workers[0].turns).toBe(2);
    expect(state.workers[0].review?.tip).toBe("tip-1");
    expect(state.workers[0].findings).toHaveLength(1);
  });

  it("restores durable merge telemetry and revert state", () => {
    const state = applyTeamSnapshot({
      workers: [{
        ...worker,
        status: "merged",
        merge: {
          commit: "merge-1",
          worker_tip: "tip-1",
          branch: "wt/agent-w1",
          target: "wt/lead",
          checks: ["workspace tests"],
          review_summary: "reviewed",
          files: 1,
          additions: 4,
          deletions: 2,
          worker_model: "scripted-a",
          worker_tokens: 10,
          cost_micros: 500_000,
          merged_at_ms: 1_787_400_000_000,
          revert_commit: "revert-1",
        },
      }],
      slots: 1,
      active: 0,
      usage: emptyProjectUsageView(),
    });
    expect(state.workers[0].merge).toMatchObject({
      commit: "merge-1",
      files: 1,
      additions: 4,
      deletions: 2,
      cost_micros: 500_000,
      revert_commit: "revert-1",
    });
  });

  it("accumulates real usage into cumulative telemetry", () => {
    const state = applyTeamEvent(
      applyTeamSnapshot({ workers: [worker], slots: 2, active: 1, usage: emptyProjectUsageView() }),
      event("usage", { prompt_tokens: 80, completion_tokens: 20, cached_tokens: 10 }),
    );
    expect(state.workers[0]).toMatchObject({
      totalTokens: 110,
      promptTokens: 88,
      completionTokens: 22,
      cachedTokens: 13,
      requests: 3,
      usedTokens: 80,
    });
  });

  it("accumulates streamed deltas for only the tagged worker", () => {
    const state = applyTeamEvent(
      applyTeamSnapshot({ workers: [worker], slots: 2, active: 1, usage: emptyProjectUsageView() }),
      event("text_delta", { text: " and done" }),
    );
    expect(state.workers[0].text).toBe("working on it and done");
  });

  it("rejects a stale generation", () => {
    const state = applyTeamSnapshot({ workers: [worker], slots: 2, active: 1, usage: emptyProjectUsageView() });
    const stale = applyTeamEvent(state, event("text_delta", { text: " stale" }, 0));
    expect(stale).toBe(state);
  });

  it("marks approvals as needs-you and clears them on finish", () => {
    let state = applyTeamSnapshot({ workers: [worker], slots: 2, active: 1, usage: emptyProjectUsageView() });
    state = applyTeamEvent(state, event("approval_request", { tool_call_id: "t1" }));
    expect(state.workers[0].needsYou).toBe(true);
    expect(state.workers[0].status).toBe("needs_you");
    state = applyTeamEvent(state, event("finished"));
    expect(state.workers[0].needsYou).toBe(false);
    expect(state.workers[0].status).toBe("completed");
  });

  it("tracks compaction completion and failure without a synthetic budget pause", () => {
    let state = applyTeamSnapshot({ workers: [worker], slots: 1, active: 1, usage: emptyProjectUsageView() });
    state = applyTeamEvent(state, event("compact_started"));
    expect(state.workers[0]).toMatchObject({ compacting: true, status: "working" });
    state = applyTeamEvent(state, event("compacted", { before: 100, after: 60 }));
    expect(state.workers[0]).toMatchObject({ compacting: false, status: "idle", error: null });
    state = applyTeamEvent(state, event("compact_started"));
    state = applyTeamEvent(state, event("error", { error: "summarizer unavailable", retrying: false }));
    expect(state.workers[0]).toMatchObject({
      compacting: false,
      status: "idle",
      error: "summarizer unavailable",
    });
  });

  it("does not turn stopped, idle, or interrupted sessions into phantom completions", () => {
    let stopped = applyTeamSnapshot({ workers: [{ ...worker, status: "stopped" }], slots: 1, active: 0, usage: emptyProjectUsageView() });
    stopped = applyTeamEvent(stopped, event("finished"));
    stopped = applyTeamEvent(stopped, event("exited"));
    expect(stopped.workers[0].status).toBe("stopped");
    let idle = applyTeamSnapshot({ workers: [{ ...worker, status: "idle" }], slots: 1, active: 1, usage: emptyProjectUsageView() });
    idle = applyTeamEvent(idle, event("exited"));
    expect(idle.workers[0].status).toBe("idle");
    const interrupted = applyTeamEvent(
      applyTeamSnapshot({ workers: [worker], slots: 1, active: 1, usage: emptyProjectUsageView() }),
      event("exited"),
    );
    expect(interrupted.workers[0]).toMatchObject({
      status: "interrupted",
      error: "the worker session exited before completing its turn",
    });
  });

  it("starts empty", () => {
    expect(initialTeamState.workers).toEqual([]);
    expect(initialTeamState.active).toBe(0);
    expect(initialTeamState.usage.costMicros).toBe(0);
  });

  it("carries the durable project usage aggregate into the view", () => {
    const state = applyTeamSnapshot({
      workers: [],
      slots: 2,
      active: 0,
      usage: {
        input_tokens: 900,
        cached_tokens: 300,
        output_tokens: 120,
        cost_micros: 1_234_567,
        unpriced_models: ["mystery-model", "mystery-model"],
      },
    });
    expect(state.usage).toEqual({
      inputTokens: 900,
      cachedTokens: 300,
      outputTokens: 120,
      costMicros: 1_234_567,
      unpricedModels: ["mystery-model"],
    });
  });

  it("normalizes a malformed usage record without fabricating a negative cost", () => {
    expect(projectUsageFrom(undefined)).toEqual({
      inputTokens: 0,
      cachedTokens: 0,
      outputTokens: 0,
      costMicros: 0,
      unpricedModels: [],
    });
    expect(
      projectUsageFrom({
        input_tokens: -5,
        cached_tokens: Number.NaN,
        output_tokens: 12.9,
        cost_micros: -1,
        unpriced_models: ["a", 7 as unknown as string, "b"],
      }),
    ).toEqual({
      inputTokens: 0,
      cachedTokens: 0,
      outputTokens: 12,
      costMicros: 0,
      unpricedModels: ["a", "b"],
    });
  });
});
