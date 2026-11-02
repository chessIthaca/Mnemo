/**
 * The popup: an info toast clears itself quickly, an error toast stays long
 * enough to be read (pausing while hovered) and can be copied.
 */

import { fireEvent, render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import type { ToastLevel } from "../lib/types";
import Toast from "./Toast.svelte";

/** Put one popup into the store, as `withToast` would. */
function showToast(message: string, level: ToastLevel): void {
  app.set({ ...fixtureState(), toast: { id: 1, message, level } });
}

describe("the popup", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    app.set(fixtureState());
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("fades an info popup out after about four seconds", async () => {
    showToast("Merged.", "info");
    render(Toast);
    expect(screen.getByText("Merged.")).toBeTruthy();

    await vi.advanceTimersByTimeAsync(4300);
    await tick();

    expect(get(app).toast).toBeNull();
  });

  it("keeps an error popup past four seconds, then dismisses it at twelve", async () => {
    showToast("Could not stop the team.", "error");
    render(Toast);
    await vi.advanceTimersByTimeAsync(4300);
    await tick();
    expect(screen.getByText("Could not stop the team.")).toBeTruthy();

    await vi.advanceTimersByTimeAsync(8000);
    await tick();
    expect(get(app).toast).toBeNull();
  });

  it("pauses the error countdown while the pointer rests on it", async () => {
    showToast("Could not stop the team.", "error");
    render(Toast);
    const popup = screen.getByRole("alert");

    await fireEvent.mouseEnter(popup);
    await vi.advanceTimersByTimeAsync(60_000);
    await tick();
    expect(get(app).toast).not.toBeNull();

    await fireEvent.mouseLeave(popup);
    await vi.advanceTimersByTimeAsync(13_000);
    await tick();
    expect(get(app).toast).toBeNull();
  });

  it("offers a copy button on an error popup only", async () => {
    showToast("Could not stop the team.", "error");
    const { unmount } = render(Toast);
    expect(screen.getByRole("button", { name: "Copy notification" })).toBeTruthy();
    unmount();

    showToast("Merged.", "info");
    render(Toast);
    expect(screen.queryByRole("button", { name: "Copy notification" })).toBeNull();
  });

  it("copies the error text and confirms it in place", async () => {
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    showToast("Could not stop the team.", "error");
    render(Toast);

    await fireEvent.click(screen.getByRole("button", { name: "Copy notification" }));
    await tick();

    expect(writeText).toHaveBeenCalledWith("Could not stop the team.");
  });

  it("is announced assertively only for an error", async () => {
    showToast("Could not stop the team.", "error");
    render(Toast);
    expect(screen.getByRole("alert").getAttribute("aria-live")).toBe("assertive");
    actions.dismissToast();
    await tick();

    showToast("Merged.", "info");
    await tick();
    expect(screen.getByRole("status").getAttribute("aria-live")).toBe("polite");
  });
});
