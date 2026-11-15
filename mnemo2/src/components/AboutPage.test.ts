/**
 * The About page's Updates row: it describes the auto-update state in one
 * line, offers a manual check whenever one can run, and stays quiet in a
 * build that cannot update itself.
 */

import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import type { AboutTransport } from "../lib/about";
import type { UpdateRestart, UpdateState, UpdatesClient } from "../lib/updates";
import AboutPage from "./AboutPage.svelte";

const IDLE: UpdateState = { phase: "idle", last_checked_ms: null };

/** The About page's shell stand-in: a version that answers. */
const transport: AboutTransport = {
  version: vi.fn(async () => "2.5.0"),
  open: vi.fn(async () => undefined),
};

/** A controllable update client with an emit channel for host events. */
function makeClient(initial: UpdateState) {
  let current = initial;
  let push: ((state: UpdateState) => void) | null = null;
  const client: UpdatesClient = {
    state: vi.fn(async () => current),
    check: vi.fn(async () => current),
    restart: vi.fn(async (_force: boolean): Promise<UpdateRestart> => ({ result: "started" })),
    onState: vi.fn(async (handler: (state: UpdateState) => void) => {
      push = handler;
      return () => {
        push = null;
      };
    }),
  };
  return {
    client,
    check: client.check,
    emit: (next: UpdateState) => {
      current = next;
      push?.(next);
    },
  };
}

/** The About page over a fake update client, with the first state delivered. */
async function page(initial: UpdateState) {
  const made = makeClient(initial);
  render(AboutPage, { props: { transport, updatesClient: made.client } });
  await waitFor(() => expect(made.client.state).toHaveBeenCalled());
  await tick();
  return made;
}

describe("the About page Updates row", () => {
  it("shows_the_update_description_and_checks_now", async () => {
    const made = await page(IDLE);
    expect(screen.getByText("Mnemo2 checks for updates automatically.")).toBeTruthy();
    const button = screen.getByRole("button", { name: "Check now" }) as HTMLButtonElement;
    expect(button.disabled).toBe(false);

    await fireEvent.click(button);
    await waitFor(() => expect(made.check).toHaveBeenCalledTimes(1));
  });

  it("disabled_build_hides_check_now", async () => {
    await page({ phase: "disabled", reason: "source_build" });
    expect(
      screen.getByText("Automatic updates are off in this build (built from source)."),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Check now" })).toBeNull();
  });

  it("check_now_disabled_while_downloading", async () => {
    const made = await page(IDLE);
    made.emit({ phase: "downloading", version: "2.6.0", downloaded: 1, total: 2 });
    await screen.findByText("Downloading Mnemo2 2.6.0… 50%");
    expect(
      (screen.getByRole("button", { name: "Check now" }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("a_failed_version_read_leaves_the_page_quiet", async () => {
    const failing: AboutTransport = {
      version: vi.fn(async () => {
        throw new Error("no shell");
      }),
      open: vi.fn(async () => undefined),
    };
    const made = makeClient(IDLE);
    render(AboutPage, { props: { transport: failing, updatesClient: made.client } });

    await screen.findByText("Mnemo2 checks for updates automatically.");
    expect(screen.queryByText("v2.5.0")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
