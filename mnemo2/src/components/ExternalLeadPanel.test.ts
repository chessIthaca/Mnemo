import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { app } from "../lib/store";
import { createWorkers, fixtureState } from "../test/fixtures";
import ExternalLeadPanel from "./ExternalLeadPanel.svelte";

function attachExternalLead(sinceMs: number | null) {
  app.update((state) => ({ ...state, externalLead: { attached: true, root: "C:/Mnemo", sinceMs } }));
}

/** The same local HH:MM the panel renders for a fixed attach time. */
function leadingSinceText(sinceMs: number) {
  const at = new Date(sinceMs);
  const hours = String(at.getHours()).padStart(2, "0");
  const minutes = String(at.getMinutes()).padStart(2, "0");
  return `Leading since ${hours}:${minutes}`;
}

describe("ExternalLeadPanel", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  it("shows the external-lead pointer with the leading-since time", () => {
    attachExternalLead(1_000);
    render(ExternalLeadPanel, { onReveal: vi.fn() });

    expect(screen.getByText("EXTERNAL LEAD")).toBeTruthy();
    expect(screen.getByText("Claude Code is leading this project")).toBeTruthy();
    expect(
      screen.getByText(
        "Plans, briefs, reviews and merges come from your Claude Code session. Talk to the lead there and follow the team's work here.",
      ),
    ).toBeTruthy();
    expect(screen.getByText(leadingSinceText(1_000))).toBeTruthy();
  });

  it("omits the leading-since line when the host reported no attach time", () => {
    attachExternalLead(null);
    render(ExternalLeadPanel, { onReveal: vi.fn() });

    expect(screen.getByText("Claude Code is leading this project")).toBeTruthy();
    expect(screen.queryByText(/Leading since/)).toBeNull();
  });

  it("lists running workers and workers that need the user as buttons that open their output panels", async () => {
    attachExternalLead(null);
    render(ExternalLeadPanel, { onReveal: vi.fn() });

    expect(screen.getByText("Working:")).toBeTruthy();
    expect(screen.getByText("Needs you:")).toBeTruthy();
    // Kai, Sol, Milo and June are dispatching, working or fixing.
    for (const name of ["Kai", "Sol", "Milo", "June"]) {
      expect(screen.getByRole("button", { name: `Show ${name}'s output` })).toBeTruthy();
    }
    // Ana, Theo and Noa wait on a human; each carries its status.
    expect(screen.getByRole("button", { name: "Open Ana" })).toBeTruthy();
    expect(screen.getByText("(In review)")).toBeTruthy();
    expect(screen.getByText("(Conflict)")).toBeTruthy();
    expect(screen.getByText("(Failed)")).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Show Kai's output" }));
    expect(get(app).workerPanelId).toBe("kai");

    await fireEvent.click(screen.getByRole("button", { name: "Open Noa" }));
    expect(get(app).workerPanelId).toBe("noa");
  });

  it("says no workers are running while the team is quiet", () => {
    attachExternalLead(null);
    const quiet = createWorkers().filter((worker) => worker.id === "ren" || worker.id === "liv");
    app.update((state) => ({ ...state, workers: quiet }));
    render(ExternalLeadPanel, { onReveal: vi.fn() });

    expect(screen.getByText("No workers running right now.")).toBeTruthy();
    expect(screen.queryByText("Working:")).toBeNull();
    expect(screen.queryByText("Needs you:")).toBeNull();
  });

  it("reveals the built-in lead through its callback", async () => {
    attachExternalLead(null);
    const onReveal = vi.fn();
    render(ExternalLeadPanel, { onReveal });

    await fireEvent.click(screen.getByRole("button", { name: "Open the built-in lead anyway" }));
    expect(onReveal).toHaveBeenCalledTimes(1);
  });
});