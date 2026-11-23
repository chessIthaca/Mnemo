import { describe, expect, it } from "vitest";
import { createAuditWorker, createWorkers } from "../test/fixtures";
import { type Worker, workerStatusText } from "./types";

const audit = (overrides: Partial<Worker>): Worker => ({ ...createAuditWorker(), ...overrides });

describe("workerStatusText", () => {
  it("shows a reported audit as Reported while its verdict is not adjudicated yet", () => {
    expect(workerStatusText(audit({ status: "completed", reportDelivered: true }))).toBe("Reported");
  });

  it("shows a used audit as Audit used once Mnemo closed its stopped session", () => {
    expect(workerStatusText(audit({ status: "stopped", reportDelivered: true }))).toBe("Audit used");
  });

  it("treats a legacy completed audit as reported, matching the finished-audit rule", () => {
    // Ledgers written before `report_delivered` existed record a finished audit
    // as `completed` (record.rs `audit_finished`), so the label follows suit.
    expect(workerStatusText(audit({ status: "completed", reportDelivered: false }))).toBe("Reported");
  });

  it("keeps an audit that has not reported on its plain status", () => {
    expect(workerStatusText(audit({ status: "working", reportDelivered: false }))).toBe("Working");
    expect(workerStatusText(audit({ status: "failed", reportDelivered: false }))).toBe("Failed");
    expect(workerStatusText(audit({ status: "stopped", reportDelivered: false }))).toBe("Stopped");
  });

  it("renders implementation workers exactly as before", () => {
    const [implementation] = createWorkers();
    expect(workerStatusText({ ...implementation, reportDelivered: true })).toBe("In review");
    expect(workerStatusText({ ...implementation, status: "completed", reportDelivered: true })).toBe("Completed");
    expect(workerStatusText({ ...implementation, status: "stopped", reportDelivered: true })).toBe("Stopped");
    expect(workerStatusText({ ...implementation, status: "merged", merge: { ...implementation.merge!, target: "wt/mnemo" } })).toBe(
      "Landed",
    );
    expect(workerStatusText({ ...implementation, status: "merged", reportDelivered: true })).toBe("Landed");
  });
});
