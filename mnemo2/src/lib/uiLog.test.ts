/**
 * The UI log's frontend half: batching, level normalization and the two
 * uncaught-error hooks.
 *
 * The host command is faked here; `src-tauri/src/mnemo2/ui_log.rs` owns the
 * file, its rotation and its no-project fallback, and tests that side itself.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const host = vi.hoisted(() => ({
  calls: [] as Array<{ command: string; args: unknown }>,
  /** Make the next `ui_log` call reject, as a failed write would. */
  failing: false,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (command: string, args?: unknown) => {
    host.calls.push({ command, args });
    if (host.failing) throw new Error("the host refused the write");
    return undefined;
  },
}));

vi.mock("./runtime", () => ({ isTauriRuntime: () => true }));

import { flushUiLog, installUiErrorLogging, logUi } from "./uiLog";

/** The entries the fake host was handed, flattened over its batches. */
function logged(): Array<Record<string, unknown>> {
  return host.calls.flatMap(
    (call) => ((call.args as { entries: Array<Record<string, unknown>> }).entries ?? []),
  );
}

describe("the UI log", () => {
  beforeEach(() => {
    host.calls = [];
    host.failing = false;
    vi.useFakeTimers();
  });

  afterEach(async () => {
    vi.useRealTimers();
    // Drain anything a test left queued so the next file starts clean.
    await flushUiLog();
  });

  it("batches a burst into one host call", async () => {
    logUi("error", "Could not stop the team.");
    logUi("info", "Merged.");

    expect(host.calls).toEqual([]);
    await vi.advanceTimersByTimeAsync(300);

    expect(host.calls).toHaveLength(1);
    expect(host.calls[0].command).toBe("ui_log");
    expect(logged().map((entry) => entry.message)).toEqual([
      "Could not stop the team.",
      "Merged.",
    ]);
  });

  it("keeps the level, the source, the route and the detail", async () => {
    logUi("error", "Boom.", { source: "window", route: "team", detail: "at foo.ts:1" });
    await flushUiLog();

    const [entry] = logged();
    expect(entry.level).toBe("error");
    expect(entry.source).toBe("window");
    expect(entry.route).toBe("team");
    expect(entry.detail).toBe("at foo.ts:1");
    expect(typeof entry.ts).toBe("number");
  });

  it("reads an unknown level as info and defaults the source", async () => {
    // A caller that passes something else still lands in the file's two levels.
    logUi("warning" as unknown as "info", "Odd.", { source: "  " });
    await flushUiLog();

    const [entry] = logged();
    expect(entry.level).toBe("info");
    expect(entry.source).toBe("ui");
    expect(entry.detail).toBeUndefined();
  });

  it("never throws when the host refuses the write", async () => {
    host.failing = true;
    expect(() => logUi("error", "Lost.")).not.toThrow();
    await expect(flushUiLog()).resolves.toBeUndefined();
    expect(logged().map((entry) => entry.message)).toEqual(["Lost."]);
  });

  it("installs no popup for an uncaught error, only a log line", async () => {
    installUiErrorLogging(window);

    const failure = new TypeError("cannot read properties of undefined");
    window.dispatchEvent(new ErrorEvent("error", { message: failure.message, error: failure }));
    await flushUiLog();

    const [entry] = logged();
    expect(entry.level).toBe("error");
    expect(entry.source).toBe("window");
    expect(entry.message).toBe("cannot read properties of undefined");
    expect(String(entry.detail)).toContain("TypeError");
  });

  it("logs an unhandled rejection with its stack", async () => {
    installUiErrorLogging(window);

    const reason = new Error("the promise gave up");
    window.dispatchEvent(
      Object.assign(new Event("unhandledrejection"), { reason }) as PromiseRejectionEvent,
    );
    await flushUiLog();

    const [entry] = logged();
    expect(entry.level).toBe("error");
    expect(entry.source).toBe("promise");
    expect(entry.message).toBe("the promise gave up");
    expect(String(entry.detail)).toContain("Error: the promise gave up");
  });

  it("describes a rejection that is not an Error", async () => {
    installUiErrorLogging(window);

    window.dispatchEvent(
      Object.assign(new Event("unhandledrejection"), { reason: "plain string" }) as
        PromiseRejectionEvent,
    );
    await flushUiLog();

    const [entry] = logged();
    expect(entry.message).toBe("plain string");
    expect(entry.detail).toBeUndefined();
  });
});
