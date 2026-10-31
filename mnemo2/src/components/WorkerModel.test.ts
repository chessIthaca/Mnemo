import { fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import type { LeadModel, Worker } from "../lib/types";
import { createWorkers, fixtureState } from "../test/fixtures";
import WorkerDetail from "./WorkerDetail.svelte";

const models: LeadModel[] = [
  { endpoint: "test", model: "fast", spec: "test/fast", is_default: true, reasoning_efforts: [] },
  { endpoint: "test", model: "deep", spec: "test/deep", is_default: false, reasoning_efforts: ["low", "high"] },
] as LeadModel[];

const worker = (status: Worker["status"]): Worker => ({
  ...createWorkers()[0],
  status,
  model: "test/fast",
  modelSpec: "test/fast",
  tier: "simple",
});

describe("worker model", () => {
  beforeEach(() => {
    const state = fixtureState();
    app.set({ ...state, leadRuntime: { ...state.leadRuntime, models } });
  });

  it("moves a stopped worker to another model from its header", async () => {
    const setWorkerModel = vi.spyOn(actions, "setWorkerModel").mockResolvedValue();
    render(WorkerDetail, { worker: worker("failed") });
    expect(screen.getByText("tier: simple")).toBeTruthy();
    const select = screen.getByRole("combobox", { name: /model$/ }) as HTMLSelectElement;
    expect(select.value).toBe("test/fast");
    await fireEvent.change(select, { target: { value: "test/deep" } });
    expect(setWorkerModel).toHaveBeenCalledWith(createWorkers()[0].id, "test/deep", null);
    setWorkerModel.mockRestore();
  });

  it("shows the model read-only while the worker is mid-turn", () => {
    render(WorkerDetail, { worker: worker("working") });
    expect(screen.queryByRole("combobox", { name: /model$/ })).toBeNull();
    expect(screen.getByTitle("Pause or stop the worker to change its model")).toBeTruthy();
  });
});
