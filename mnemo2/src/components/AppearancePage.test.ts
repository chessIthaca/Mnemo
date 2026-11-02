import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import AppearancePage from "./AppearancePage.svelte";
import type { AppearancePatch, AppearanceSettings, AppearanceTransport } from "../lib/sounds";

/** The stored snapshot the page opens on: sounds on, the light palette. */
function snapshot(overrides: Partial<AppearanceSettings> = {}): AppearanceSettings {
  return {
    sound_complete: true,
    sound_input_needed: true,
    sound_stopped_errors: true,
    theme: "light",
    ...overrides,
  };
}

/** A transport over an in-memory snapshot, with spies on both calls. */
function makeTransport(overrides: Partial<AppearanceSettings> = {}) {
  let stored = snapshot(overrides);
  const get = vi.fn(async () => ({ ...stored }));
  const save = vi.fn(async (patch: AppearancePatch) => {
    stored = { ...stored, ...patch };
    return { ...stored };
  });
  const transport = { get, save } as unknown as AppearanceTransport;
  return { transport, get, save, stored: () => stored };
}

async function page(overrides: Partial<AppearanceSettings> = {}) {
  const made = makeTransport(overrides);
  render(AppearancePage, { props: { transport: made.transport } });
  await screen.findByRole("radio", { name: /Dark/ });
  return made;
}

beforeEach(() => {
  vi.clearAllMocks();
  document.documentElement.removeAttribute("data-theme");
  document.documentElement.style.colorScheme = "";
  window.localStorage.clear();
});

describe("AppearancePage theme", () => {
  it("checks the stored theme and paints nothing until the user chooses", async () => {
    await page({ theme: "light" });
    expect((screen.getByRole("radio", { name: /Light/ }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("radio", { name: /Dark/ }) as HTMLInputElement).checked).toBe(false);
    expect((screen.getByRole("radio", { name: /System/ }) as HTMLInputElement).checked).toBe(false);
    expect(document.documentElement.dataset.theme).toBeUndefined();
  });

  it("paints the chosen palette at once and saves it as [ui] theme", async () => {
    const made = await page({ theme: "light" });
    await fireEvent.click(screen.getByRole("radio", { name: /Dark/ }));

    await waitFor(() => expect(made.save).toHaveBeenCalledWith({ theme: "dark" }));
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(made.stored().theme).toBe("dark");
    await screen.findByText("Saved. Every screen repaints — no restart.");
  });

  it("paints back to the stored palette when the save is refused", async () => {
    const made = await page({ theme: "light" });
    made.save.mockRejectedValueOnce(new Error("appearance_settings_save: disk full"));
    await fireEvent.click(screen.getByRole("radio", { name: /Dark/ }));

    await screen.findByRole("alert");
    expect(screen.getByRole("alert").textContent).toContain("disk full");
    expect(document.documentElement.dataset.theme, "the failed save paints the old palette back").toBe("light");
    expect(made.stored().theme).toBe("light");
    expect((screen.getByRole("radio", { name: /Light/ }) as HTMLInputElement).checked).toBe(true);
  });

  it("stores system and follows the OS palette", async () => {
    const made = await page({ theme: "light" });
    await fireEvent.click(screen.getByRole("radio", { name: /System/ }));
    await waitFor(() => expect(made.save).toHaveBeenCalledWith({ theme: "system" }));
    // jsdom has no matchMedia, so `system` resolves to light here.
    expect(document.documentElement.dataset.theme).toBe("light");
  });
});
