import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";
import type { SafetySettings, SafetyTransport } from "../lib/safetySettings";
import SafetyPage from "./SafetyPage.svelte";

/**
 * SafetyPage — the approval mode, the project's auto-approve rules, and the
 * machine-global git commands that always ask, each on its own Save button.
 * These tests cover the git list: what it shows, what a save sends, the
 * inline refusal of a malformed entry, and Discard.
 */

/** The stored snapshot the page opens on: asking mode, no rules, the default git list. */
function snapshot(overrides: Partial<SafetySettings> = {}): SafetySettings {
  return {
    mode: "approve-each-action",
    rules: [],
    git_approval: ["merge", "push"],
    project_open: true,
    live_lead: true,
    ...overrides,
  };
}

/** A transport over an in-memory snapshot, with a spy on the git save. */
function makeTransport(overrides: Partial<SafetySettings> = {}) {
  let stored = snapshot(overrides);
  const get = vi.fn(async () => structuredClone(stored));
  const saveMode = vi.fn(async (mode: SafetySettings["mode"]) => {
    stored = { ...stored, mode };
    return structuredClone(stored);
  });
  const saveRules = vi.fn(async (rules: SafetySettings["rules"]) => {
    stored = { ...stored, rules };
    return structuredClone(stored);
  });
  const saveGitApproval = vi.fn(async (operations: string[]) => {
    stored = { ...stored, git_approval: [...operations] };
    return structuredClone(stored);
  });
  const transport = { get, saveMode, saveRules, saveGitApproval } as SafetyTransport;
  return { transport, get, saveGitApproval, stored: () => stored };
}

/** Render the page and wait for the snapshot to land. */
async function page(overrides: Partial<SafetySettings> = {}) {
  const made = makeTransport(overrides);
  render(SafetyPage, { props: { transport: made.transport } });
  await screen.findByRole("region", { name: "Git commands that always ask" });
  return made;
}

/** The git field (its label names it, unlike the section's same-named heading). */
function gitField(): HTMLInputElement {
  return screen.getByRole("textbox", { name: "Git commands that always ask" }) as HTMLInputElement;
}

/** The git section, so its Discard is not confused with the mode's or the rules'. */
function gitSection(): HTMLElement {
  return screen.getByRole("region", { name: "Git commands that always ask" });
}

describe("SafetyPage git approval list", () => {
  it("shows the saved git list", async () => {
    await page();
    expect(gitField().value).toBe("merge, push");
  });

  it("saves an edited git list", async () => {
    const made = await page();
    await fireEvent.input(gitField(), { target: { value: "merge, push, rebase" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save git commands" }));

    await waitFor(() => expect(made.saveGitApproval).toHaveBeenCalledWith(["merge", "push", "rebase"]));
    expect(made.stored().git_approval).toEqual(["merge", "push", "rebase"]);
    expect(gitField().value).toBe("merge, push, rebase");
  });

  it("refuses a malformed entry inline", async () => {
    const made = await page();
    await fireEvent.input(gitField(), { target: { value: "push --force" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save git commands" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("--force");
    expect(made.saveGitApproval, "the invalid list never reaches the transport").not.toHaveBeenCalled();
  });

  it("discard restores the saved list", async () => {
    await page();
    await fireEvent.input(gitField(), { target: { value: "merge, push, rebase" } });
    expect(gitField().value).toBe("merge, push, rebase");

    await fireEvent.click(within(gitSection()).getByRole("button", { name: "Discard" }));
    expect(gitField().value).toBe("merge, push");
  });
});
