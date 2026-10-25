import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import ControlRoom from "./ControlRoom.svelte";

const workerRows = () => screen.getAllByRole("button", { name: /Open .* worker thread/ });

describe("Mission Control", () => {
  beforeEach(() => {
    app.set(fixtureState());
    actions.openTeam();
  });

  it("renders ten workers and filters the larger team", async () => {
    render(ControlRoom);
    expect(workerRows()).toHaveLength(10);

    await fireEvent.click(screen.getByRole("button", { name: /Needs you/ }));
    expect(workerRows()).toHaveLength(3);

    await fireEvent.click(screen.getByRole("button", { name: /Done/ }));
    expect(workerRows()).toHaveLength(2);

    await fireEvent.click(screen.getByRole("button", { name: /All/ }));
    expect(workerRows()).toHaveLength(10);
  });

  it("opens a task on its thread with its model and workflow", () => {
    render(ControlRoom);
    expect(screen.getByRole("button", { name: /^Thread/ }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getAllByText("kimi-k3.1-pro").length).toBeGreaterThan(0);
    expect(screen.getByRole("list", { name: /Task workflow:/ })).toBeTruthy();
  });

  it("resets the worker detail to Thread when switching tasks", async () => {
    render(ControlRoom);
    await fireEvent.click(screen.getByRole("button", { name: /^Review/ }));
    expect(screen.getByRole("button", { name: /^Review/ }).getAttribute("aria-pressed")).toBe("true");

    await fireEvent.click(screen.getByRole("button", { name: /Open Theo worker thread/ }));
    expect(screen.getByRole("button", { name: /^Thread/ }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: /^Review/ }).getAttribute("aria-pressed")).toBe("false");
  });

  it("gates merge on real evidence without simulating review actions", async () => {
    render(ControlRoom);
    await fireEvent.click(screen.getByRole("button", { name: /^Review/ }));
    const merge = screen.getByRole("button", { name: "Merge reviewed branch" }) as HTMLButtonElement;
    expect(merge.disabled).toBe(true);

    await fireEvent.click(
      screen.getByRole("button", { name: "Fix tiny finding: Give the merge action an accessible name" }),
    );
    expect(screen.getByLabelText("Existing text")).toBeTruthy();
    expect(screen.getByLabelText("Replacement")).toBeTruthy();
    expect(screen.queryByText(/mock fixer/i)).toBeNull();

    app.update((state) => ({
      ...state,
      workers: state.workers.map((worker) =>
        worker.id === "ana"
          ? {
              ...worker,
              findings: worker.findings.map((finding) => ({
                ...finding,
                status: "fixed" as const,
                evidence: "verified on the fix commit",
              })),
            }
          : worker,
      ),
    }));
    await vi.waitFor(() => expect(merge.disabled).toBe(false));
    await fireEvent.click(merge);
    expect(get(app).toast?.message).toMatch(/team runtime/i);
    expect(get(app).workers.find((worker) => worker.id === "ana")?.merge).toBeUndefined();
  });

  it("renders worker thread messages as sanitized Markdown", async () => {
    const workers = fixtureState().workers;
    workers[0].messages = [{
      id: "markdown-worker",
      from: "worker",
      body: "**Implemented** with `team_dispatch`.",
      time: "now",
    }];
    app.set(fixtureState());
    app.update((state) => ({ ...state, workers }));
    actions.openTeam();

    render(ControlRoom);
    const strong = await screen.findByText("Implemented");
    expect(strong.tagName).toBe("STRONG");
    expect(screen.getByText("team_dispatch").tagName).toBe("CODE");
    expect(screen.queryByText(/\*\*Implemented\*\*/)).toBeNull();
  });

  it("does not expose Diff as a per-worker action", async () => {
    render(ControlRoom);
    await fireEvent.click(screen.getByRole("button", { name: /Open Theo worker thread/ }));
    expect(screen.queryByRole("button", { name: "Diff" })).toBeNull();
  });
});
