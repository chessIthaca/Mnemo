import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import ControlRoom from "./ControlRoom.svelte";

const workerRows = () => screen.getAllByRole("button", { name: /Open .* worker thread/ });

describe("Mission Control", () => {
  beforeEach(() => {
    app.set(fixtureState());
    actions.openTeam();
  });

  it("starts no work itself: the header, the list and the output, with no dispatch row", () => {
    // No open plan makes the old dispatch bar fall back to a placeholder line.
    app.update((state) => ({ ...state, board: [] }));
    render(ControlRoom);
    expect(screen.getByRole("heading", { name: "Mission Control" })).toBeTruthy();
    expect(screen.getByText(/\d+ live \/ \d+ slots/)).toBeTruthy();
    expect(workerRows().length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Resize worker list" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Dispatch/ })).toBeNull();
    expect(screen.queryByLabelText("Plan")).toBeNull();
    expect(screen.queryByLabelText("Plan step")).toBeNull();
    expect(screen.queryByLabelText("New worker brief")).toBeNull();
    expect(screen.queryByText(/No open plan/)).toBeNull();
  });

  describe("list/output split", () => {
    const originalInnerWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");

    const layout = () => document.querySelector(".control-layout") as HTMLElement;
    const columns = () => layout().getAttribute("style") ?? "";
    const divider = () => screen.getByRole("button", { name: "Resize worker list" });

    beforeEach(() => {
      Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1600 });
      app.update((state) => ({ ...state, boardWidth: null }));
    });

    afterEach(() => {
      if (originalInnerWidth) Object.defineProperty(window, "innerWidth", originalInnerWidth);
    });

    it("keeps today's proportion until the divider moves", () => {
      render(ControlRoom);
      expect(columns()).toContain("--board-columns: minmax(300px, 0.88fr) 9px minmax(420px, 1.12fr)");
      expect(divider().getAttribute("aria-label")).toBe("Resize worker list");
    });

    it("resizes the list with the arrow keys and clamps at both bounds", async () => {
      app.update((state) => ({ ...state, boardWidth: 600 }));
      render(ControlRoom);
      expect(columns()).toContain("--board-columns: 600px 9px minmax(420px, 1fr)");

      await fireEvent.keyDown(divider(), { key: "ArrowRight" });
      expect(get(app).boardWidth).toBe(624);
      await fireEvent.keyDown(divider(), { key: "ArrowLeft" });
      await fireEvent.keyDown(divider(), { key: "ArrowLeft" });
      expect(get(app).boardWidth).toBe(576);

      app.update((state) => ({ ...state, boardWidth: 1171 }));
      await fireEvent.keyDown(divider(), { key: "ArrowRight" });
      expect(get(app).boardWidth).toBe(1171);
      app.update((state) => ({ ...state, boardWidth: 300 }));
      await fireEvent.keyDown(divider(), { key: "ArrowLeft" });
      expect(get(app).boardWidth).toBe(300);
    });

    it("drags the divider with the pointer", async () => {
      app.update((state) => ({ ...state, boardWidth: 600 }));
      render(ControlRoom);
      await fireEvent.pointerDown(divider(), { clientX: 600, pointerId: 1 });
      await fireEvent.pointerMove(window, { clientX: 700, pointerId: 1 });
      expect(get(app).boardWidth).toBe(700);
      await fireEvent.pointerMove(window, { clientX: 400, pointerId: 1 });
      expect(get(app).boardWidth).toBe(400);
      await fireEvent.pointerUp(window, { clientX: 400, pointerId: 1 });
      await fireEvent.pointerMove(window, { clientX: 900, pointerId: 1 });
      expect(get(app).boardWidth).toBe(400);
    });

    it("resets to today's proportion on double-click", async () => {
      app.update((state) => ({ ...state, boardWidth: 720 }));
      render(ControlRoom);
      await fireEvent.doubleClick(divider());
      expect(get(app).boardWidth).toBeNull();
      expect(columns()).toContain("0.88fr");
    });
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

  it("opens the selected worker's real Diff view", async () => {
    render(ControlRoom);
    await fireEvent.click(screen.getByRole("button", { name: /Open Theo worker thread/ }));
    await fireEvent.click(screen.getByRole("button", { name: "Diff" }));
    expect(get(app).workbenchTab).toBe("diff");
    expect(get(app).diffWorkerId).toBe("theo");
    expect(get(app).route).toBe("lead");
  });
});
