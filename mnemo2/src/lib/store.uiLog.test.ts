/**
 * Every popup reaches the UI log with the level its call site chose.
 *
 * The store is the one door every popup goes through (`withToast`), so this
 * checks the door: an ordinary confirmation is `info`, a failure or a refusal
 * is `error`, and the popup carries the same level for the toast's own timing.
 */

import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fixtureState } from "../test/fixtures";

const { logUi } = vi.hoisted(() => ({ logUi: vi.fn() }));
vi.mock("./uiLog", () => ({ logUi: (...args: unknown[]) => logUi(...args) }));

import { actions, app } from "./store";

describe("popups reach the UI log", () => {
  beforeEach(() => {
    logUi.mockClear();
    app.set(fixtureState());
  });

  it("logs a plain confirmation as info", () => {
    actions.showToast("Merged.");

    expect(logUi).toHaveBeenCalledTimes(1);
    expect(logUi).toHaveBeenCalledWith("info", "Merged.", {
      source: "toast",
      route: "lead",
    });
    expect(get(app).toast).toMatchObject({ message: "Merged.", level: "info" });
  });

  it("logs a failure as error", () => {
    actions.showToast("Could not stop the team: the host refused.", "error");

    expect(logUi).toHaveBeenCalledWith("error", "Could not stop the team: the host refused.", {
      source: "toast",
      route: "lead",
    });
    expect(get(app).toast).toMatchObject({ level: "error" });
  });

  it("marks a refused action as an error at its call site", async () => {
    await actions.openProject("no-such-project");

    expect(logUi).toHaveBeenCalledWith("error", "That project is not registered.", {
      source: "toast",
      route: "lead",
    });
    expect(get(app).toast).toMatchObject({ level: "error" });
  });

  it("marks a missing native lead as an error", async () => {
    await expect(actions.interruptLead()).rejects.toThrow(/not connected/);

    expect(logUi).toHaveBeenCalledWith(
      "error",
      "The native lead is not connected, so it cannot be interrupted.",
      { source: "toast", route: "lead" },
    );
  });
});
