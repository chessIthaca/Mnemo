import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TeamTransport } from "./teamEngine";
import {
  emptyProjectUsageView,
  findingFromWire,
  type TeamFinding,
  type TeamMergeRecord,
  type TeamWorkerRecord,
} from "./teamState";
import type { ReviewEvidence } from "./types";

const team = vi.hoisted(() => ({
  transport: null as TeamTransport | null,
}));

vi.mock("./teamEngine", () => ({
  createTeamTransport: () => team.transport,
}));
vi.mock("./leadEngine", () => ({
  createLeadTransport: () => null,
}));
vi.mock("./projectEngine", () => ({
  createProjectTransport: () => ({
    attach: async () => ({ conflict_pid: null, conflict_started_at: null }),
    preferences: async () => ({
      workbench_open: false,
      workbench_tab: "browser",
      workbench_width: 480,
      browser_url: "about:blank",
      diff_worker_id: null,
    }),
  }),
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

function nativeWorker(overrides: Partial<TeamWorkerRecord> = {}): TeamWorkerRecord {
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
    ...overrides,
  };
}

function makeTransport(initial: TeamWorkerRecord) {
  let current = initial;
  const transport: TeamTransport = {
    snapshot: vi.fn(async () => ({ workers: [current], slots: 3, active: 1, usage: emptyProjectUsageView() })),
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
    setSlots: vi.fn(async () => ({ workers: [current], slots: 3, active: 1, usage: emptyProjectUsageView() })),
    compact: vi.fn(async () => current),
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

    const evidence: ReviewEvidence = {
      unanchoredPass: true,
      intent: "record the intent-driven review",
      execution: "the action serializes evidence for team_review",
      integration: "the merge gate reads the same record",
      testQuality: "the transport payload is asserted below",
      residualRisk: "host validation remains authoritative",
    };
    await actions.reviewWorker("ana", "delta review", [findingFromWire(finding)], evidence);
    await actions.recordWorkerCheck("ana", "workspace tests", "passed", "ok");
    await actions.fixFinding("ana", "F-1", "old", "new");
    await actions.spawnFixer("ana", "F-1");
    await actions.resolveFinding("ana", "F-1", "fixed", "verified");
    await actions.wontfixFinding("ana", "F-1", "justified separately");
    await actions.compactWorker("ana");

    expect(transport.compact).toHaveBeenCalledWith("ana");
    expect(transport.review).toHaveBeenCalledWith("ana", "delta review", [finding], {
      unanchored_pass: true,
      intent: "record the intent-driven review",
      execution: "the action serializes evidence for team_review",
      integration: "the merge gate reads the same record",
      test_quality: "the transport payload is asserted below",
      residual_risk: "host validation remains authoritative",
    });
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

  it("maps a legacy worker record to an implementation worker with no evidence", async () => {
    const transport = makeTransport(nativeWorker());
    await openNativeTeam(transport);

    expect(get(app).workers[0]).toMatchObject({
      kind: "implementation",
      reviewRisk: "standard",
      auditTarget: undefined,
      auditTip: undefined,
    });
    expect(get(app).workers[0].review?.tip).toBe("tip-1");
    expect(get(app).workers[0].review?.evidence).toBeUndefined();
  });

  it("maps a high-risk audit worker and its review evidence into the UI", async () => {
    const transport = makeTransport(nativeWorker({
      id: "audit-1",
      name: "Ada",
      kind: "audit",
      review_risk: "high",
      audit_target: "ana",
      audit_tip: "tip-1",
      review: {
        summary: "audited tip-1",
        tip: "tip-1",
        reviewed_at_ms: 1_787_400_000_000,
        evidence: {
          unanchored_pass: true,
          intent: "intent",
          execution: "execution",
          integration: "integration",
          test_quality: "test quality",
          residual_risk: "residual risk",
          independent_reviewer_id: "audit-1",
          audit_adjudication: "accepted",
        },
      },
    }));
    await openNativeTeam(transport);

    expect(get(app).workers[0]).toMatchObject({
      kind: "audit",
      reviewRisk: "high",
      auditTarget: "ana",
      auditTip: "tip-1",
      review: {
        tip: "tip-1",
        evidence: {
          unanchoredPass: true,
          intent: "intent",
          execution: "execution",
          integration: "integration",
          testQuality: "test quality",
          residualRisk: "residual risk",
          independentReviewerId: "audit-1",
          auditAdjudication: "accepted",
        },
      },
    });
  });
});
