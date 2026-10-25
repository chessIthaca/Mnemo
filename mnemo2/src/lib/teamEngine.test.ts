import { describe, expect, it, vi } from "vitest";
import { createTauriTeamTransport, TEAM_EVENT_CHANNEL } from "./teamEngine";

describe("teamEngine", () => {
  it("dispatches a worker through team_dispatch", async () => {
    const invoke = vi.fn(async () => ({ id: "w1" }));
    const transport = createTauriTeamTransport({
      invoke: invoke as never,
      listen: vi.fn(async () => () => {}) as never,
    });
    await transport.dispatch("do it", "test/scripted-a");
    expect(invoke).toHaveBeenCalledWith("team_dispatch", {
      brief: "do it",
      model: "test/scripted-a",
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
