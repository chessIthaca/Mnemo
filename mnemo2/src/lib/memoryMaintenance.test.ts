import { describe, expect, it, vi } from "vitest";
import { isTauriRuntime } from "./runtime";

vi.mock("./runtime", () => ({ isTauriRuntime: vi.fn(() => false) }));

import {
  applyMaintenanceEvent,
  createMaintenanceTransport,
  createTauriMaintenanceTransport,
  idleOp,
  MAINTENANCE_CHANNEL,
  MAINTENANCE_OPS,
  progressLabel,
  type MaintenanceEvent,
  type MaintenanceTransportBindings,
} from "./memoryMaintenance";

/** Recording bindings: every invoke/listen call is a spy answering `ok`. */
function bindings() {
  const invoked: Array<{ command: string; args: unknown }> = [];
  let handler: ((event: { payload: MaintenanceEvent }) => void) | null = null;
  const listen = vi.fn(async (_channel: string, cb: (event: { payload: MaintenanceEvent }) => void) => {
    handler = cb;
    return () => {};
  });
  const invoke = vi.fn(async (command: string, args?: unknown) => {
    invoked.push({ command, args });
  });
  const bound = { invoke, listen } as unknown as MaintenanceTransportBindings;
  return { bound, invoked, listen, push: (event: MaintenanceEvent) => handler?.({ payload: event }) };
}

describe("maintenance events", () => {
  it("starts an action afresh, clearing the previous result", () => {
    const finished = { ...idleOp(), summary: "3 session(s) consolidated", phase: "vacuuming" };
    expect(applyMaintenanceEvent(finished, { type: "started", op: "cleanup" })).toEqual({
      ...idleOp(),
      running: true,
    });
  });

  it("copies a progress tick and drops a stale result", () => {
    const failed = { ...idleOp(), running: false, error: "boom", phase: "" };
    expect(
      applyMaintenanceEvent(failed, {
        type: "progress",
        op: "rebuild",
        done: 50,
        total: 200,
        phase: "embedding",
      }),
    ).toEqual({
      running: true,
      done: 50,
      total: 200,
      phase: "embedding",
      summary: null,
      error: null,
    });
  });

  it("ends on done with the summary shown and no error", () => {
    const running = { ...idleOp(), running: true, done: 2, total: 2, phase: "consolidating" };
    expect(
      applyMaintenanceEvent(running, { type: "done", op: "cleanup", summary: "2 session(s)" }),
    ).toEqual({
      running: false,
      done: 2,
      total: 2,
      phase: "",
      summary: "2 session(s)",
      error: null,
    });
  });

  it("ends on failed with the error shown and no summary", () => {
    const running = { ...idleOp(), running: true, phase: "indexing" };
    expect(
      applyMaintenanceEvent(running, { type: "failed", op: "index", error: "no space left" }),
    ).toEqual({
      running: false,
      done: 0,
      total: 0,
      phase: "",
      summary: null,
      error: "no space left",
    });
  });
});

describe("progressLabel", () => {
  it("counts units when the stage has them", () => {
    expect(progressLabel({ ...idleOp(), running: true, done: 50, total: 200, phase: "embedding" })).toBe(
      "embedding 50/200",
    );
  });

  it("shows the bare stage label when it has no units", () => {
    expect(progressLabel({ ...idleOp(), running: true, phase: "vacuuming" })).toBe("vacuuming");
  });

  it("says working before the first tick", () => {
    expect(progressLabel({ ...idleOp(), running: true })).toBe("working");
  });

  it("says nothing when the action is idle", () => {
    expect(progressLabel({ ...idleOp(), summary: "done" })).toBe("");
  });
});

describe("maintenance transport", () => {
  it("runs each action through its own literal command", async () => {
    const { bound, invoked } = bindings();
    const transport = createTauriMaintenanceTransport(bound);

    await transport.run("cleanup");
    await transport.run("rebuild");
    await transport.run("index");

    expect(invoked.map((call) => call.command)).toEqual([
      "memory_cleanup",
      "memory_rebuild_search",
      "memory_rebuild_index",
    ]);
  });

  it("subscribes on the maintenance channel and forwards the event payload", async () => {
    const { bound, listen, push } = bindings();
    const seen: MaintenanceEvent[] = [];
    const transport = createTauriMaintenanceTransport(bound);

    await transport.onEvent((event) => seen.push(event));
    expect(listen).toHaveBeenCalledWith(MAINTENANCE_CHANNEL, expect.any(Function));

    push({ type: "started", op: "cleanup" });
    expect(seen).toEqual([{ type: "started", op: "cleanup" }]);
  });

  it("has no transport outside the desktop app", () => {
    vi.mocked(isTauriRuntime).mockReturnValue(false);
    expect(createMaintenanceTransport()).toBeNull();
  });
});

describe("MAINTENANCE_OPS", () => {
  it("lists clean up, rebuild search and rebuild index in that order", () => {
    expect(MAINTENANCE_OPS.map((entry) => entry.op)).toEqual(["cleanup", "rebuild", "index"]);
    expect(MAINTENANCE_OPS.map((entry) => entry.button)).toEqual(["Clean up", "Rebuild", "Rebuild"]);
  });
});
