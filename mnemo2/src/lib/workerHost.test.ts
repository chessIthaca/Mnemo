import { describe, expect, it } from "vitest";
import type { Worker, WorkerHost, WorkerStatus } from "./types";
import { ACTIVE_HOST_STATUSES, hostAway, hostBadgeTitle, hostFromRemote } from "./workerHost";

/** Just the two fields the host helpers read. */
const workerWith = (
  host: WorkerHost | null,
  status: WorkerStatus,
): Pick<Worker, "host" | "status"> => ({ host, status });

const host = (overrides: Partial<WorkerHost> = {}): WorkerHost => ({
  name: "studio",
  os: "macos",
  offlineSinceMs: null,
  ssh: null,
  ...overrides,
});

describe("hostAway", () => {
  const now = 1_787_400_125_000;

  it("is null for a worker on this computer, however active it is", () => {
    expect(hostAway(workerWith(null, "working"), now)).toBeNull();
  });

  it("is null while the host is connected", () => {
    expect(hostAway(workerWith(host(), "working"), now)).toBeNull();
  });

  it("counts the seconds a working worker's host has been away", () => {
    const awayHost = host({ offlineSinceMs: now - 125_000 });
    expect(hostAway(workerWith(awayHost, "working"), now)).toBe(125);
  });

  it("counts for every active status", () => {
    const awayHost = host({ offlineSinceMs: now - 5_000 });
    for (const status of ACTIVE_HOST_STATUSES) {
      expect(hostAway(workerWith(awayHost, status), now)).toBe(5);
    }
  });

  it("keeps a merged worker quiet while its host is still away", () => {
    const awayHost = host({ offlineSinceMs: now - 600_000 });
    expect(hostAway(workerWith(awayHost, "merged"), now)).toBeNull();
    expect(hostAway(workerWith(awayHost, "completed"), now)).toBeNull();
    expect(hostAway(workerWith(awayHost, "failed"), now)).toBeNull();
    expect(hostAway(workerWith(awayHost, "stopped"), now)).toBeNull();
  });

  it("never reports a negative age when the clocks disagree", () => {
    const awayHost = host({ offlineSinceMs: now + 30_000 });
    expect(hostAway(workerWith(awayHost, "working"), now)).toBe(0);
  });
});

describe("hostBadgeTitle", () => {
  it("names the computer and its OS", () => {
    expect(hostBadgeTitle({ host: host() })).toBe("Runs on studio (macOS)");
  });

  it("adds the SSH connection a Claude Code session runs over", () => {
    expect(hostBadgeTitle({ host: host({ os: "windows", ssh: "studio-ssh" }) })).toBe(
      "Runs on studio (Windows), Claude Code session over SSH studio-ssh",
    );
  });

  it("is empty for a worker on this computer", () => {
    expect(hostBadgeTitle({ host: null })).toBe("");
  });
});

describe("hostFromRemote", () => {
  it("maps a remote checkout onto the worker's host", () => {
    expect(
      hostFromRemote({
        node_id: "a".repeat(32),
        node_name: "studio",
        os: "macos",
        project: "0123456789ab",
        checkout: "mnemo-w1",
        path: "/Users/dev/Mnemo/checkouts/mnemo-w1",
        synced_tip: "abc1234",
        offline_since_ms: 1_787_400_050_000,
        ssh: "studio-ssh",
      }),
    ).toEqual({
      name: "studio",
      os: "macos",
      offlineSinceMs: 1_787_400_050_000,
      ssh: "studio-ssh",
    });
  });

  it("leaves a connected host and a Mnemo worker's missing link empty", () => {
    expect(
      hostFromRemote({
        node_id: "b".repeat(32),
        node_name: "laptop",
        os: "windows",
        project: "0123456789ab",
        checkout: "mnemo-w2",
        path: "C:/Mnemo/checkouts/mnemo-w2",
      }),
    ).toEqual({ name: "laptop", os: "windows", offlineSinceMs: null, ssh: null });
  });

  it("is null for a worker on this computer", () => {
    expect(hostFromRemote(undefined)).toBeNull();
    expect(hostFromRemote(null)).toBeNull();
  });
});
