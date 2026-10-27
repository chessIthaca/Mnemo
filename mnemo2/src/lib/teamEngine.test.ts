import { describe, expect, it, vi } from "vitest";
import { createTauriTeamTransport, TEAM_EVENT_CHANNEL } from "./teamEngine";

describe("teamEngine", () => {
  it("dispatches a worker through team_dispatch", async () => {
    const invoke = vi.fn(async () => ({ id: "w1" }));
    const transport = createTauriTeamTransport({
      invoke: invoke as never,
      listen: vi.fn(async () => () => {}) as never,
    });
    await transport.dispatch("do it", "test/scripted-a", "worker0001");
    expect(invoke).toHaveBeenCalledWith("team_dispatch", {
      brief: "do it",
      model: "test/scripted-a",
      workerId: "worker0001",
    });
  });

  it("sends worker messages with the camelCase command args", async () => {
    const invoke = vi.fn(async () => ({ id: "w1" }));
    const transport = createTauriTeamTransport({
      invoke: invoke as never,
      listen: vi.fn(async () => () => {}) as never,
    });
    await transport.message("w1", "status?");
    expect(invoke).toHaveBeenCalledWith("team_message", {
      workerId: "w1",
      text: "status?",
    });
  });

  it("maps every Phase 3 operation to its native command", async () => {
    const invoke = vi.fn(async () => ({ id: "w1" }));
    const transport = createTauriTeamTransport({
      invoke: invoke as never,
      listen: vi.fn(async () => () => {}) as never,
    });
    const finding = {
      id: "F-1",
      severity: "low" as const,
      title: "Example",
      detail: "Detail",
      file: "src/lib.rs",
      line: 1,
      status: "open" as const,
      fix_commit: null,
      evidence: null,
      justification: null,
    };
    await transport.review("w1", "summary", [finding]);
    await transport.recordCheck("w1", "workspace tests", "passed", "ok");
    await transport.resolveFinding("w1", "F-1", "fixed", "verified", null);
    await transport.reviewFix("w1", "F-1", "src/lib.rs", "old", "new");
    await transport.routeFix("w1", "F-1");
    await transport.merge("w1");
    await transport.revertMerge("w1");
    await transport.stopAll();
    await transport.compact("w1");

    expect(invoke).toHaveBeenNthCalledWith(1, "team_review", {
      workerId: "w1",
      summary: "summary",
      findings: [finding],
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "team_record_check", {
      workerId: "w1",
      name: "workspace tests",
      status: "passed",
      detail: "ok",
    });
    expect(invoke).toHaveBeenNthCalledWith(3, "team_resolve_finding", {
      workerId: "w1",
      findingId: "F-1",
      status: "fixed",
      evidence: "verified",
      justification: null,
    });
    expect(invoke).toHaveBeenNthCalledWith(4, "team_review_fix", {
      workerId: "w1",
      findingId: "F-1",
      path: "src/lib.rs",
      oldString: "old",
      newString: "new",
    });
    expect(invoke).toHaveBeenNthCalledWith(5, "team_route_fix", {
      workerId: "w1",
      findingId: "F-1",
    });
    expect(invoke).toHaveBeenNthCalledWith(6, "team_merge", { workerId: "w1" });
    expect(invoke).toHaveBeenNthCalledWith(7, "team_revert_merge", { workerId: "w1" });
    expect(invoke).toHaveBeenNthCalledWith(8, "team_stop_all");
    expect(invoke).toHaveBeenNthCalledWith(9, "team_compact", { workerId: "w1" });
  });

  it("subscribes to the team event channel", async () => {
    let seenChannel = "";
    const listen = vi.fn(async (channel: string, handler: (event: unknown) => void) => {
      seenChannel = channel;
      handler({ payload: { worker_id: "w1", generation: 1 } });
      return () => {};
    });
    const transport = createTauriTeamTransport({
      invoke: vi.fn(async () => ({})) as never,
      listen: listen as never,
    });
    const handler = vi.fn();
    await transport.onEvent(handler);
    expect(seenChannel).toBe(TEAM_EVENT_CHANNEL);
    expect(handler).toHaveBeenCalledWith({ worker_id: "w1", generation: 1 });
  });
});
