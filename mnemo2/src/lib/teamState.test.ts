import { describe, expect, it } from "vitest";
import {
  applyTeamEvent,
  applyTeamSnapshot,
  initialTeamState,
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
  checks: [],
  error: null,
};

const event = (kind: string, extra: Record<string, unknown> = {}, generation = 1): TeamEvent => ({
  worker_id: "w1",
  generation,
  event: { agent: 1, event: { kind, ...extra } },
});

describe("teamState", () => {
  it("restores real workers from a snapshot", () => {
    const state = applyTeamSnapshot({ workers: [worker], slots: 2, active: 1 });
    expect(state.workers).toHaveLength(1);
    expect(state.workers[0]).toMatchObject({
      id: "w1",
      text: "working on it",
      status: "working",
      usedTokens: 10,
    });
    expect(state.active).toBe(1);
  });

  it("accumulates streamed deltas for only the tagged worker", () => {
    const state = applyTeamEvent(
      applyTeamSnapshot({ workers: [worker], slots: 2, active: 1 }),
      event("text_delta", { text: " and done" }),
    );
    expect(state.workers[0].text).toBe("working on it and done");
  });

  it("rejects a stale generation", () => {
    const state = applyTeamSnapshot({ workers: [worker], slots: 2, active: 1 });
    const stale = applyTeamEvent(state, event("text_delta", { text: " stale" }, 0));
    expect(stale).toBe(state);
  });

  it("marks approvals as needs-you and clears them on finish", () => {
    let state = applyTeamSnapshot({ workers: [worker], slots: 2, active: 1 });
    state = applyTeamEvent(state, event("approval_request", { tool_call_id: "t1" }));
    expect(state.workers[0].needsYou).toBe(true);
    expect(state.workers[0].status).toBe("needs_you");
    state = applyTeamEvent(state, event("finished"));
    expect(state.workers[0].needsYou).toBe(false);
    expect(state.workers[0].status).toBe("completed");
  });

  it("starts empty", () => {
    expect(initialTeamState.workers).toEqual([]);
    expect(initialTeamState.active).toBe(0);
  });
});
