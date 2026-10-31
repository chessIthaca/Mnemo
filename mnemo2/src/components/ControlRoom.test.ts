import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import type { BoardPlan } from "../lib/boardState";
import { fixtureState } from "../test/fixtures";
import ControlRoom from "./ControlRoom.svelte";

const workerRows = () => screen.getAllByRole("button", { name: /Open .* worker thread/ });

const plan = (id: string, title: string, steps: [number, string | null, string, boolean][]): BoardPlan => ({
  id,
  title,
  goal: "",
  kind: "feature",
  status: "open",
  closed_at_ms: null,
  report_path: null,
  steps: steps.map(([number, header, text, done]) => ({ number, header, text, done, workers: [] })),
});
const boardPlans = [
  plan("plan-a", "Plan A", [
    [1, "Engine", "Build the engine", true],
    [2, null, "Wire the UI", false],
    [3, "Polish", "Tidy", false],
  ]),
  plan("plan-b", "Plan B", [[1, "Only", "One", false]]),
  { ...plan("plan-c", "Plan C", [[1, null, "Shipped", true]]), status: "closed" as const },
];

describe("Mission Control", () => {
  beforeEach(() => {
    const state = fixtureState();
    state.board = boardPlans;
    app.set(state);
    actions.openTeam();
  });

  it("lists open plans and their undone steps, defaulting to the first", async () => {
    render(ControlRoom);
    const planSelect = screen.getByLabelText("Plan") as HTMLSelectElement;
    expect([...planSelect.options].map((o) => o.text)).toEqual(["Plan A", "Plan B"]);
    expect(planSelect.value).toBe("plan-a");
    const stepSelect = screen.getByLabelText("Plan step") as HTMLSelectElement;
    expect([...stepSelect.options].map((o) => o.text)).toEqual(["2. Wire the UI", "3. Polish"]);
    expect(stepSelect.value).toBe("2");
    await fireEvent.change(planSelect, { target: { value: "plan-b" } });
    expect([...(screen.getByLabelText("Plan step") as HTMLSelectElement).options].map((o) => o.text)).toEqual(["1. Only"]);
  });

  it("passes the plan and step to the transport action, keeps the brief on refusal", async () => {
    const dispatch = vi.spyOn(actions, "dispatchWorker").mockResolvedValue(false);
    render(ControlRoom);
    const button = screen.getByRole("button", { name: "Dispatch" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    const brief = screen.getByLabelText("New worker brief") as HTMLInputElement;
    await fireEvent.input(brief, { target: { value: "wire it" } });
    expect(button.disabled).toBe(false);
    await fireEvent.change(screen.getByLabelText("Plan step"), { target: { value: "3" } });
    await fireEvent.click(button);
    expect(dispatch).toHaveBeenCalledWith("wire it", "plan-a", 3, null, null);
    expect(brief.value).toBe("wire it");
  });

  it("disables dispatch when the plan has no undone step", async () => {
    app.update((state) => ({ ...state, board: [plan("plan-d", "Plan D", [[1, null, "Done", true]])] }));
    render(ControlRoom);
    await fireEvent.input(screen.getByLabelText("New worker brief"), { target: { value: "x" } });
    expect((screen.getByRole("button", { name: "Dispatch" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("replaces the form with one line when no plan is open", () => {
    app.update((state) => ({ ...state, board: [boardPlans[2]] }));
    render(ControlRoom);
    expect(screen.getByText("No open plan — ask the lead to plan the work first.")).toBeTruthy();
    expect(screen.queryByLabelText("New worker brief")).toBeNull();
  });

  it("dispatches with an explicit model and reasoning effort", async () => {
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        models: [
          {
            endpoint: "test",
            model: "scripted-a",
            spec: "test/scripted-a",
            is_default: true,
            reasoning_efforts: ["max", "low", "off"],
            default_effort: "max",
          },
          {
            endpoint: "other",
            model: "scripted-a",
            spec: "other/scripted-a",
            is_default: false,
            reasoning_efforts: ["max", "low", "off"],
            default_effort: "max",
          },
        ],
      },
    }));
    const dispatch = vi.spyOn(actions, "dispatchWorker").mockResolvedValue(true);
    render(ControlRoom);
    const model = screen.getByLabelText("Worker model") as HTMLSelectElement;
    expect(model.value).toBe("test/scripted-a");
    await fireEvent.change(model, { target: { value: "other/scripted-a" } });
    await fireEvent.change(screen.getByLabelText("Worker reasoning effort"), { target: { value: "low" } });
    await fireEvent.input(screen.getByLabelText("New worker brief"), { target: { value: "write the parser" } });
    await fireEvent.click(screen.getByRole("button", { name: "Dispatch" }));
    expect(dispatch).toHaveBeenCalledWith("write the parser", "plan-a", 2, "other/scripted-a", "low");
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
