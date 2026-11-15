/**
 * The update card: it stays hidden until a release is ready, shows the
 * ready version, asks before interrupting running workers, keeps working
 * when the restart fails, and comes back for the next version.
 */

import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { actions } from "../lib/store";
import type { UpdateRestart, UpdateState, UpdatesClient } from "../lib/updates";
import UpdateBanner from "./UpdateBanner.svelte";

const IDLE: UpdateState = { phase: "idle", last_checked_ms: null };
const READY: UpdateState = { phase: "ready", version: "2.6.0", notes: null };

/** A controllable stand-in host: a fixed state plus an emit channel. */
function makeClient(initial: UpdateState) {
  let current = initial;
  let push: ((state: UpdateState) => void) | null = null;
  const restart = vi.fn(async (_force: boolean): Promise<UpdateRestart> => ({ result: "started" }));
  const client: UpdatesClient = {
    state: vi.fn(async () => current),
    check: vi.fn(async () => current),
    restart,
    onState: vi.fn(async (handler: (state: UpdateState) => void) => {
      push = handler;
      return () => {
        push = null;
      };
    }),
  };
  return {
    client,
    restart,
    emit: (next: UpdateState) => {
      current = next;
      push?.(next);
    },
  };
}

/** The banner over a fake host, with the first state already delivered. */
async function banner(initial: UpdateState) {
  const made = makeClient(initial);
  render(UpdateBanner, { props: { client: made.client } });
  await waitFor(() => expect(made.client.state).toHaveBeenCalled());
  await tick();
  return made;
}

describe("the update banner", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("hidden_until_ready", async () => {
    const made = await banner(IDLE);
    expect(screen.queryByRole("status")).toBeNull();

    made.emit({ phase: "checking" });
    await tick();
    expect(screen.queryByRole("status")).toBeNull();

    made.emit({ phase: "downloading", version: "2.6.0", downloaded: 10, total: 100 });
    await tick();
    expect(screen.queryByRole("status")).toBeNull();

    made.emit(READY);
    await waitFor(() => expect(screen.getByRole("status")).toBeTruthy());
  });

  it("shows_the_ready_version", async () => {
    await banner(READY);
    const card = screen.getByRole("status");
    expect(card.getAttribute("aria-live")).toBe("polite");
    expect(card.textContent).toContain("Mnemo2 2.6.0 is ready.");
    expect(screen.getByRole("button", { name: "Restart now" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Later" })).toBeTruthy();
  });

  it("restart_now_calls_restart_without_force", async () => {
    const made = await banner(READY);
    await fireEvent.click(screen.getByRole("button", { name: "Restart now" }));
    expect(made.restart).toHaveBeenCalledTimes(1);
    expect(made.restart).toHaveBeenCalledWith(false);
  });

  it("running_workers_ask_first_then_restart_anyway_forces", async () => {
    const made = await banner(READY);
    made.restart.mockResolvedValueOnce({ result: "workers_running", count: 2 });
    await fireEvent.click(screen.getByRole("button", { name: "Restart now" }));
    await screen.findByText(
      "2 worker(s) are still running. Restarting interrupts them; their branches and transcripts are kept.",
    );

    await fireEvent.click(screen.getByRole("button", { name: "Restart anyway" }));
    expect(made.restart).toHaveBeenNthCalledWith(1, false);
    expect(made.restart).toHaveBeenNthCalledWith(2, true);
  });

  it("keep_working_returns_to_the_card", async () => {
    const made = await banner(READY);
    made.restart.mockResolvedValueOnce({ result: "workers_running", count: 3 });
    await fireEvent.click(screen.getByRole("button", { name: "Restart now" }));
    await screen.findByText(/are still running/);

    await fireEvent.click(screen.getByRole("button", { name: "Keep working" }));
    await screen.findByText("Mnemo2 2.6.0 is ready.");
    expect(screen.getByRole("button", { name: "Restart now" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Restart anyway" })).toBeNull();
    expect(made.restart).toHaveBeenCalledTimes(1);
  });

  it("later_hides_until_another_version", async () => {
    const made = await banner(READY);
    await fireEvent.click(screen.getByRole("button", { name: "Later" }));
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());

    made.emit(READY);
    await tick();
    expect(screen.queryByRole("status")).toBeNull();

    made.emit({ phase: "ready", version: "2.7.0", notes: null });
    await screen.findByText("Mnemo2 2.7.0 is ready.");
  });

  it("a_failed_restart_toasts", async () => {
    const made = await banner(READY);
    const toast = vi.spyOn(actions, "showToast");
    made.restart.mockRejectedValueOnce("update_restart: no update is ready");

    await fireEvent.click(screen.getByRole("button", { name: "Restart now" }));

    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith("update_restart: no update is ready", "error"),
    );
    await screen.findByText("Mnemo2 2.6.0 is ready.");
    toast.mockRestore();
  });

  it("buttons_lock_while_a_restart_is_pending", async () => {
    const made = await banner(READY);
    let answer!: (value: UpdateRestart) => void;
    made.restart.mockImplementationOnce(
      () =>
        new Promise<UpdateRestart>((resolve) => {
          answer = resolve;
        }),
    );
    await fireEvent.click(screen.getByRole("button", { name: "Restart now" }));

    await waitFor(() => {
      expect((screen.getByRole("button", { name: "Restart now" }) as HTMLButtonElement).disabled).toBe(true);
      expect((screen.getByRole("button", { name: "Later" }) as HTMLButtonElement).disabled).toBe(true);
    });

    answer({ result: "started" });
    await waitFor(() => {
      expect((screen.getByRole("button", { name: "Restart now" }) as HTMLButtonElement).disabled).toBe(false);
      expect((screen.getByRole("button", { name: "Later" }) as HTMLButtonElement).disabled).toBe(false);
    });
    expect(made.restart).toHaveBeenCalledWith(false);
  });
});
