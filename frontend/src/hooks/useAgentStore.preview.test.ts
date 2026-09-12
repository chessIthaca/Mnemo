// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect, beforeEach } from "vitest";
import { useAgentStore } from "./useAgentStore";
import type { AgentId } from "../lib/types";

function resetStore(): void {
  useAgentStore.setState({
    agents: {},
    agentNames: {},
    agentParents: {},
    agentModels: {},
    agentProviders: {},
    workflowStates: {},
    activeAgent: null,
    lastDiff: null,
    planDiffs: [],
    topPlanId: null,
    selectedDiffPath: null,
    planVersion: 0,
  });
}

describe("approval_request preview (UI H3)", () => {
  beforeEach(resetStore);

  it("stores Rust ApprovalPreview on pendingApproval", () => {
    const id = 1 as AgentId;
    useAgentStore.getState().handleAgentEvent({
      agent_id: id,
      event: {
        kind: "approval_request",
        tool_call_id: "c1",
        tool_name: "file_edit",
        args: { path: "a.txt", old_string: "x", new_string: "y" },
        preview: {
          kind: "diff",
          path: "C:/proj/a.txt",
          diff: "--- a.txt\n+++ a.txt\n-x\n+y\n",
        },
        core_operation: false,
      },
    });
    const pending = useAgentStore.getState().agents[id]?.pendingApproval;
    expect(pending).not.toBeNull();
    expect(pending?.preview?.kind).toBe("diff");
    expect(pending?.preview?.diff).toContain("+y");
    expect(pending?.preview?.path).toBe("C:/proj/a.txt");
  });

  it("carries unifiedDiff into lastDiff on tool_result", () => {
    const id = 1 as AgentId;
    useAgentStore.getState().handleAgentEvent({
      agent_id: id,
      event: {
        kind: "tool_call_start",
        index: 0,
        id: "c1",
        name: "file_edit",
      },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: id,
      event: {
        kind: "tool_call_arg_delta",
        index: 0,
        fragment: '{"path":"a.txt","old_string":"x","new_string":"y"}',
      },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: id,
      event: {
        kind: "approval_request",
        tool_call_id: "c1",
        tool_name: "file_edit",
        args: { path: "a.txt", old_string: "x", new_string: "y" },
        preview: {
          kind: "diff",
          path: "a.txt",
          diff: "--- a.txt\n+++ a.txt\n-x\n+y\n",
        },
        core_operation: false,
      },
    });
    useAgentStore.getState().handleAgentEvent({
      agent_id: id,
      event: {
        kind: "tool_result",
        tool_call_id: "c1",
        result: { success: true, output: "edited a.txt" },
      },
    });
    const last = useAgentStore.getState().lastDiff;
    expect(last).not.toBeNull();
    expect(last?.unifiedDiff).toContain("+y");
    expect(last?.path).toBe("a.txt");
    expect(useAgentStore.getState().agents[id]?.pendingApproval).toBeNull();
  });
});


describe("auto-reveal panel behaviors (Phase 3c)", () => {
  beforeEach(() => {
    useAgentStore.setState({
      rightPanelVisible: false,
      rightPanelTab: "plan",
      disabledTabs: [],
    });
  });

  it("autoRevealPlan reveals panel and selects plan when hidden and enabled", () => {
    useAgentStore.getState().autoRevealPlan();
    const s = useAgentStore.getState();
    expect(s.rightPanelVisible).toBe(true);
    expect(s.rightPanelTab).toBe("plan");
  });

  it("autoRevealPlan is a no-op when panel is already visible", () => {
    useAgentStore.setState({ rightPanelVisible: true, rightPanelTab: "diff" });
    useAgentStore.getState().autoRevealPlan();
    const s = useAgentStore.getState();
    expect(s.rightPanelVisible).toBe(true);
    expect(s.rightPanelTab).toBe("diff"); // unchanged
  });

  it("autoRevealPlan is a no-op when plan tab is disabled", () => {
    useAgentStore.setState({ disabledTabs: ["plan"] });
    useAgentStore.getState().autoRevealPlan();
    const s = useAgentStore.getState();
    expect(s.rightPanelVisible).toBe(false);
    expect(s.rightPanelTab).toBe("plan");
  });

  it("autoRevealDiff reveals panel and selects diff when hidden and enabled", () => {
    useAgentStore.getState().autoRevealDiff();
    const s = useAgentStore.getState();
    expect(s.rightPanelVisible).toBe(true);
    expect(s.rightPanelTab).toBe("diff");
  });

  it("autoRevealDiff is a no-op when panel is visible", () => {
    useAgentStore.setState({ rightPanelVisible: true, rightPanelTab: "plan" });
    useAgentStore.getState().autoRevealDiff();
    const s = useAgentStore.getState();
    expect(s.rightPanelVisible).toBe(true);
    expect(s.rightPanelTab).toBe("plan");
  });

  it("autoRevealDiff is a no-op when diff tab is disabled", () => {
    useAgentStore.setState({ disabledTabs: ["diff"] });
    useAgentStore.getState().autoRevealDiff();
    const s = useAgentStore.getState();
    expect(s.rightPanelVisible).toBe(false);
  });
});

describe("applyBacklogChanged (Phase 3c)", () => {
  beforeEach(() => {
    useAgentStore.setState({ backlog: [], autoFeed: false, runAll: { active: false, done: 0, total: 0, compacting: false, concurrency: 1, spawned: [], note: null } });
  });

  it("sets backlog, autoFeed, and runAll from payload", () => {
    const payload = {
      items: [{ id: 1, text: "x", images: [], status: "pending", created_at: 0, note: null }],
      auto_feed: true,
      run_all: { active: true, done: 2, total: 5 },
    } as const;
    useAgentStore.getState().applyBacklogChanged(payload as any);
    const s = useAgentStore.getState();
    expect(s.backlog.length).toBe(1);
    expect(s.backlog[0].id).toBe(1);
    expect(s.autoFeed).toBe(true);
    expect(s.runAll).toEqual({ active: true, done: 2, total: 5 });
  });
});
