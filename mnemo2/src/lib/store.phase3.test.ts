import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TeamTransport } from "./teamEngine";
import {
  findingFromWire,
  type TeamFinding,
  type TeamMergeRecord,
  type TeamWorkerRecord,
} from "./teamState";

const team = vi.hoisted(() => ({
  transport: null as TeamTransport | null,
}));

vi.mock("./teamEngine", () => ({
  createTeamTransport: () => team.transport,
}));
vi.mock("./leadEngine", () => ({
  createLeadTransport: () => null,
}));
vi.mock("./runtime", () => ({
  isTauriRuntime: () => true,
  probeMnemo2Host: async () => true,
}));

import { actions, app } from "./store";
import { fixtureState } from "../test/fixtures";

const finding: TeamFinding = {
  id: "F-1",
  severity: "low",
  title: "Rename the function",
  detail: "Use the behavior-specific name",
  file: "src/lib.rs",
  line: 1,
  status: "open",
  fix_commit: null,
  evidence: null,
  justification: null,
};

const merge: TeamMergeRecord = {
  commit: "merge-1",
  worker_tip: "tip-1",
  branch: "wt/agent-ana",
  target: "wt/lead",
  checks: ["workspace tests"],
  review_summary: "reviewed current tip",
  files: 1,
  additions: 4,
  deletions: 2,
  worker_model: "scripted-a",
  worker_tokens: 123,
  cost_micros: 500_000,
  merged_at_ms: 1_787_400_000_000,
  revert_commit: null,
};

function nativeWorker(): TeamWorkerRecord {
  return {
    id: "ana",
    name: "Ana",
    brief: "Implement the real task",
    model: "test/scripted-a",
    model_id: "scripted-a",
    branch: "wt/agent-ana",
    worktree: "C:/repo/.worktrees/agent-ana",
    status: "in_review",
    generation: 1,
    transcript: {
      messages: [
        { role: "user", content: "Implement the real task" },
        { role: "assistant", content: [{ type: "text", text: "Implemented" }] },
      ],
    },
    pending: null,
    used_tokens: 123,
    max_tokens: 1000,
    token_budget: 200_000,
    total_tokens: 123,
    prompt_tokens: 100,
    completion_tokens: 23,
    cached_tokens: 10,
    requests: 4,
    cost_micros: 500_000,
    created_at_ms: 1_787_399_935_000,
    elapsed_ms: 65_000,
    turns: 4,
    last_activity_ms: 1_787_400_000_000,
    checks: [{ name: "workspace tests", status: "passed", detail: "ok", tip: "tip-1" }],
    findings: [finding],
    review: { summary: "reviewed current tip", tip: "tip-1", reviewed_at_ms: 1_787_400_000_000 },
    fixer_finding_id: null,
    merge: null,
    error: null,
  };
}

function makeTransport(initial: TeamWorkerRecord) {
  let current = initial;
  const transport: TeamTransport = {
    snapshot: vi.fn(async () => ({ workers: [current], slots: 3, active: 1 })),
    dispatch: vi.fn(async () => current),
    message: vi.fn(async () => current),
    pause: vi.fn(async () => current),
    interrupt: vi.fn(async () => current),
    stop: vi.fn(async () => current),
    resume: vi.fn(async () => current),
    review: vi.fn(async () => current),
    recordCheck: vi.fn(async () => current),
    resolveFinding: vi.fn(async () => current),
    reviewFix: vi.fn(async () => current),
    routeFix: vi.fn(async () => current),
    merge: vi.fn(async () => {
      current = { ...current, status: "merged", merge };
      return current;
    }),
    revertMerge: vi.fn(async () => {
      current = {
        ...current,
        merge: { ...merge, revert_commit: "revert-1" },
      };
      return current;
    }),
    stopAll: vi.fn(async () => {}),
    setTokenBudget: vi.fn(async (_workerId, tokenBudget) => {
      current = { ...current, token_budget: tokenBudget };
      return current;
    }),
    messages: vi.fn(async () => []),
    onEvent: vi.fn(async () => () => {}),
  };
  return transport;
}

async function openNativeTeam(transport: TeamTransport) {
  team.transport = transport;
  actions.openProject("mnemo");
  await vi.waitFor(() => expect(get(app).workers).toHaveLength(1));
}

describe("native Phase 3 store integration", () => {
  beforeEach(() => {
    team.transport = null;
    app.set(fixtureState());
  });

  afterEach(() => {
    actions.closeProject();
    team.transport = null;
  });

  it("maps durable workers, inline review/merge cards, revert, and stop-all", async () => {
    const transport = makeTransport(nativeWorker());
    await openNativeTeam(transport);

    const ana = get(app).workers[0];
    expect(ana).toMatchObject({
      tokens: 123,
      tokenBudget: 200_000,
      cost: 0.5,
      elapsed: "1m",
      turns: 4,
      review: { tip: "tip-1" },
    });
    expect(ana.findings[0].id).toBe("F-1");
    expect(ana.messages.map((message) => message.body)).toEqual([
      "Implement the real task",
      "Implemented",
    ]);
    expect(get(app).leadEntries.some(
      (entry) => entry.kind === "review" && entry.body === "reviewed current tip",
    )).toBe(true);

    await actions.mergeWorker("ana");
    expect(transport.merge).toHaveBeenCalledWith("ana");
    const mergeEntry = get(app).leadEntries.find((entry) => entry.kind === "merge");
    expect(mergeEntry).toMatchObject({
      kind: "merge",
      record: {
        commit: "merge-1",
        tokens: 123,
        cost: 0.5,
      },
    });

    await actions.revertMerge("ana");
    expect(transport.revertMerge).toHaveBeenCalledWith("ana");
    expect(get(app).workers[0].merge?.revertCommit).toBe("revert-1");
    expect(get(app).leadEntries.find((entry) => entry.kind === "merge")).toMatchObject({
      kind: "merge",
      record: { revertCommit: "revert-1" },
    });

    await actions.stopAll();
    expect(transport.stopAll).toHaveBeenCalled();
  });

  it("routes every finding and check operation through the real transport", async () => {
    const transport = makeTransport(nativeWorker());
    await openNativeTeam(transport);

    await actions.reviewWorker("ana", "delta review", [findingFromWire(finding)]);
    await actions.recordWorkerCheck("ana", "workspace tests", "passed", "ok");
    await actions.fixFinding("ana", "F-1", "old", "new");
    await actions.spawnFixer("ana", "F-1");
    await actions.resolveFinding("ana", "F-1", "fixed", "verified");
    await actions.wontfixFinding("ana", "F-1", "justified separately");
    await actions.setWorkerBudget("ana", 250_000);

    expect(transport.setTokenBudget).toHaveBeenCalledWith("ana", 250_000);
    expect(get(app).workers[0].tokenBudget).toBe(250_000);
    expect(transport.review).toHaveBeenCalledWith("ana", "delta review", [finding]);
    expect(transport.recordCheck).toHaveBeenCalledWith(
      "ana",
      "workspace tests",
      "passed",
      "ok",
    );
    expect(transport.reviewFix).toHaveBeenCalledWith(
      "ana",
      "F-1",
      "src/lib.rs",
      "old",
      "new",
    );
    expect(transport.routeFix).toHaveBeenCalledWith("ana", "F-1");
    expect(transport.resolveFinding).toHaveBeenNthCalledWith(
      1,
      "ana",
      "F-1",
      "fixed",
      "verified",
      null,
    );
    expect(transport.resolveFinding).toHaveBeenNthCalledWith(
      2,
      "ana",
      "F-1",
      "wontfix",
      null,
      "justified separately",
    );
  });
});
