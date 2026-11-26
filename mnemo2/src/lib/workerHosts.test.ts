import { describe, expect, it } from "vitest";
import { checksLabel, hostStateLabel, osLabel, type WorkerHost } from "./workerHosts";

function host(over: Partial<WorkerHost> = {}): WorkerHost {
  return {
    name: "studio",
    node_id: "n1",
    os: "windows",
    connected: true,
    slots: 2,
    busy: 1,
    catalog_ready: true,
    checks: "passed",
    last_seen_ms: 1_000,
    ssh: null,
    claude_home: null,
    ...over,
  };
}

describe("hostStateLabel", () => {
  it("says connected", () => {
    expect(hostStateLabel(host(), 5_000)).toBe("connected");
  });
  it("says away since a relative time", () => {
    expect(hostStateLabel(host({ connected: false }), 1_000 + 125_000)).toBe("away since 2 min ago");
    expect(hostStateLabel(host({ connected: false }), 1_000 + 30_000)).toBe("away since 30 s ago");
  });
  it("says never connected without a last seen time", () => {
    expect(hostStateLabel(host({ connected: false, last_seen_ms: null }), 5_000)).toBe("never connected");
  });
});

describe("checksLabel", () => {
  it("maps the three states", () => {
    expect(checksLabel("passed").tone).toBe("ok");
    expect(checksLabel("pending")).toEqual({ text: "checking…", tone: "pending" });
    const bad = checksLabel("failed: no git");
    expect(bad.tone).toBe("bad");
    expect(bad.text).toContain("no git");
    expect(bad.text).not.toContain("failed: failed");
  });
});

describe("osLabel", () => {
  it("names the known systems", () => {
    expect(osLabel("windows")).toBe("Windows");
    expect(osLabel("macos")).toBe("macOS");
    expect(osLabel("plan9")).toBe("plan9");
  });
});
