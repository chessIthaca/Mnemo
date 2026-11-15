/**
 * The update client: every description branch, the age buckets, when a
 * manual check may run, the host commands it invokes, and the browser
 * stand-in it falls back to.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { isTauriRuntime } from "./runtime";

vi.mock("./runtime", () => ({ isTauriRuntime: vi.fn(() => false) }));

import {
  UPDATE_EVENT_CHANNEL,
  ago,
  canCheck,
  createUpdatesClient,
  describeUpdateState,
} from "./updates";

/** Point `isTauriRuntime` at the current test. */
function setTauri(value: boolean): void {
  vi.mocked(isTauriRuntime).mockReturnValue(value);
}

describe("the update description", () => {
  it("describe_each_phase", () => {
    const now = 1_700_000_000_000;
    expect(describeUpdateState({ phase: "disabled", reason: "source_build" }, now)).toBe(
      "Automatic updates are off in this build (built from source).",
    );
    expect(describeUpdateState({ phase: "disabled", reason: "preview" }, now)).toBe(
      "Updates are checked by the desktop app.",
    );
    expect(describeUpdateState({ phase: "disabled", reason: "endpoint_blocked" }, now)).toBe(
      "Automatic updates are off: endpoint_blocked.",
    );
    expect(describeUpdateState({ phase: "idle", last_checked_ms: null }, now)).toBe(
      "Mnemo2 checks for updates automatically.",
    );
    expect(describeUpdateState({ phase: "idle", last_checked_ms: now - 120_000 }, now)).toBe(
      "Up to date · checked 2 min ago",
    );
    expect(describeUpdateState({ phase: "checking" }, now)).toBe("Checking for updates…");
    expect(
      describeUpdateState(
        { phase: "downloading", version: "2.6.0", downloaded: 512, total: 2048 },
        now,
      ),
    ).toBe("Downloading Mnemo2 2.6.0… 25%");
    expect(
      describeUpdateState(
        { phase: "downloading", version: "2.6.0", downloaded: 512, total: null },
        now,
      ),
    ).toBe("Downloading Mnemo2 2.6.0…");
    expect(describeUpdateState({ phase: "ready", version: "2.6.0", notes: null }, now)).toBe(
      "Mnemo2 2.6.0 is ready. Restart to update.",
    );
    expect(
      describeUpdateState(
        { phase: "failed", message: "offline", last_checked_ms: now - 60_000 },
        now,
      ),
    ).toBe("Could not check for updates: offline. Mnemo2 tries again later.");
  });

  it("ago_buckets", () => {
    expect(ago(59_999)).toBe("just now");
    expect(ago(60_000)).toBe("1 min ago");
    expect(ago(3_600_000)).toBe("1 h ago");
    expect(ago(86_400_000)).toBe("1 d ago");
  });

  it("can_check_only_idle_or_failed", () => {
    expect(canCheck({ phase: "idle", last_checked_ms: null })).toBe(true);
    expect(canCheck({ phase: "failed", message: "offline", last_checked_ms: 1 })).toBe(true);
    expect(canCheck({ phase: "disabled", reason: "source_build" })).toBe(false);
    expect(canCheck({ phase: "checking" })).toBe(false);
    expect(canCheck({ phase: "downloading", version: "2.6.0", downloaded: 1, total: 2 })).toBe(
      false,
    );
    expect(canCheck({ phase: "ready", version: "2.6.0", notes: null })).toBe(false);
  });
});

describe("the updates client", () => {
  beforeEach(() => {
    setTauri(false);
  });

  it("client_invokes_the_host_commands", async () => {
    setTauri(true);
    const invoke = vi.fn(async (command: string, _args?: Record<string, unknown>) => {
      if (command === "update_restart") return { result: "started" };
      return { phase: "idle", last_checked_ms: 1 };
    });
    const listen = vi.fn(
      async (_channel: string, handler: (event: { payload: unknown }) => void) => {
        handler({ payload: { phase: "checking" } });
        return () => {};
      },
    );
    const client = createUpdatesClient({
      invoke: invoke as unknown as typeof import("@tauri-apps/api/core").invoke,
      listen: listen as unknown as typeof import("@tauri-apps/api/event").listen,
    });

    await client.state();
    await client.check();
    await client.restart(true);
    const seen: unknown[] = [];
    await client.onState((state) => seen.push(state));

    expect(invoke.mock.calls.map((call) => [call[0], call[1]])).toEqual([
      ["update_state", undefined],
      ["update_check", undefined],
      ["update_restart", { force: true }],
    ]);
    expect(listen).toHaveBeenCalledWith(UPDATE_EVENT_CHANNEL, expect.any(Function));
    expect(seen).toEqual([{ phase: "checking" }]);
  });

  it("client_outside_tauri_is_disabled", async () => {
    const client = createUpdatesClient();

    await expect(client.state()).resolves.toEqual({ phase: "disabled", reason: "preview" });
    await expect(client.check()).resolves.toEqual({ phase: "disabled", reason: "preview" });
    await expect(client.restart(false)).rejects.toThrow("Updates need the desktop app.");

    const seen: unknown[] = [];
    const stop = await client.onState((state) => seen.push(state));
    expect(typeof stop).toBe("function");
    stop();
    expect(seen).toEqual([]);
  });
});
