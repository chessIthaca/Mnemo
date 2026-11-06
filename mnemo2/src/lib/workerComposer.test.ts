import { describe, expect, it } from "vitest";
import type { Worker } from "./types";
import { workerBox } from "./workerComposer";

/** The box only reads these fields; the rest of the Worker is noise here. */
function box(overrides: Partial<Pick<Worker, "name" | "status" | "kind" | "reportDelivered" | "compacting">> = {}) {
  return workerBox({ name: "Kai", status: "idle", kind: "implementation", ...overrides });
}

describe("workerBox", () => {
  it("closes the box for a merged worker", () => {
    const result = box({ status: "merged" });
    expect(result.mode).toBe("done");
    expect(result.canSend).toBe(false);
    expect(result.placeholder).toBe("Kai is done");
  });

  it("closes the box while a worker is merging", () => {
    const result = box({ status: "merging" });
    expect(result.mode).toBe("done");
    expect(result.canSend).toBe(false);
    expect(result.placeholder).toBe("Kai is done");
  });

  it("closes the box for a cancelled worker", () => {
    const result = box({ status: "cancelled" });
    expect(result.mode).toBe("done");
    expect(result.canSend).toBe(false);
    expect(result.placeholder).toBe("Kai is done");
  });

  it("closes the box for an audit that delivered its report", () => {
    const result = box({ kind: "audit", status: "working", reportDelivered: true });
    expect(result.mode).toBe("done");
    expect(result.canSend).toBe(false);
    expect(result.placeholder).toBe("Kai finished its audit");
  });

  it("closes the box for an audit that completed", () => {
    const result = box({ kind: "audit", status: "completed" });
    expect(result.mode).toBe("done");
    expect(result.canSend).toBe(false);
    expect(result.placeholder).toBe("Kai finished its audit");
  });

  it("points at Resume for a stopped worker", () => {
    const result = box({ status: "stopped" });
    expect(result.mode).toBe("blocked");
    expect(result.canSend).toBe(false);
    expect(result.placeholder).toBe("Resume Kai to message it");
  });

  it("points at Resume for an interrupted worker", () => {
    const result = box({ status: "interrupted" });
    expect(result.mode).toBe("blocked");
    expect(result.canSend).toBe(false);
    expect(result.placeholder).toBe("Resume Kai to message it");
  });

  it("blocks the box while an idle worker compacts", () => {
    const result = box({ status: "idle", compacting: true });
    expect(result.mode).toBe("blocked");
    expect(result.canSend).toBe(false);
    expect(result.placeholder).toBe("Kai is compacting");
  });

  it("steers a working worker and offers Pause", () => {
    const result = box({ status: "working" });
    expect(result.mode).toBe("steer");
    expect(result.canSend).toBe(true);
    expect(result.placeholder).toBe("Steer Kai…");
    expect(result.sendLabel).toBe("Steer Kai");
    expect(result.canPause).toBe(true);
  });

  it("steers a dispatching worker and offers Pause", () => {
    const result = box({ status: "dispatching" });
    expect(result.mode).toBe("steer");
    expect(result.canPause).toBe(true);
  });

  it("steers a fixing worker without offering Pause", () => {
    const result = box({ status: "fixing" });
    expect(result.mode).toBe("steer");
    expect(result.canSend).toBe(true);
    expect(result.canPause).toBe(false);
  });

  it("messages an idle worker", () => {
    const result = box({ status: "idle" });
    expect(result.mode).toBe("message");
    expect(result.canSend).toBe(true);
    expect(result.placeholder).toBe("Message Kai");
    expect(result.sendLabel).toBe("Send message to Kai");
  });

  it("closes the box for a completed implementation worker", () => {
    const result = box({ status: "completed" });
    expect(result.mode).toBe("done");
    expect(result.canSend).toBe(false);
    expect(result.placeholder).toBe("Kai is done");
  });

  it("closes the box for an in-review worker", () => {
    const result = box({ status: "in_review" });
    expect(result.mode).toBe("done");
    expect(result.canSend).toBe(false);
    expect(result.placeholder).toBe("Kai is done");
  });

  it("keeps the box open for a conflicted worker", () => {
    expect(box({ status: "conflict" }).mode).toBe("message");
  });

  it("keeps the box open for a failed worker", () => {
    expect(box({ status: "failed" }).mode).toBe("message");
  });

  it("keeps the box open for a paused worker", () => {
    expect(box({ status: "paused" }).mode).toBe("message");
  });

  it("never offers Pause outside the running statuses", () => {
    for (const status of ["idle", "conflict", "failed", "paused"] as const) {
      expect(box({ status }).canPause).toBe(false);
    }
  });
});

