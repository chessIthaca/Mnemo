import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";
import type { ModelSettings, ModelSettingsSection, ModelSettingsTransport } from "../lib/modelSettings";
import SavingsPage from "./SavingsPage.svelte";

/**
 * SavingsPage — the optimizer levers with their knobs, saved as the
 * "optimizer" section on its own button. Model prices are set with each
 * model on the Endpoints page, so this page only points there.
 */

function sampleSettings(): ModelSettings {
  return {
    task_models: { summarize: null },
    vision_model: null,
    embedding: { selected: "hash", status: "keyword_only", options: [] },
    optimizer: {
      delta_reads: true,
      compress_output: true,
      archive: true,
      compaction_survival: true,
      quality_score: true,
      lean_output_nudge: true,
      recall_delta: false,
      idle_compaction: true,
      archive_min_chars: 2400,
      compress_min_chars: 3000,
      lean_output_fill_pct: 80,
      nudge_cooldown_requests: 4,
      compress_extra_commands: ["cargo test", "npm run build"],
    },
    budget: { enabled: false, max_tokens_per_plan: 0, max_escalations_per_plan: 0, max_retries_per_lane: 0 },
  };
}

function makeTransport(overrides: Partial<ModelSettingsTransport> = {}) {
  const settings = sampleSettings();
  const base = {
    get: vi.fn(async () => structuredClone(settings)),
    save: vi.fn(async (section: ModelSettingsSection, value: unknown) => undefined),
    install: vi.fn(async () => undefined),
    onEmbedderEvent: vi.fn(async () => () => {}),
  };
  return { settings, transport: { ...base, ...overrides } as typeof base };
}

function page(overrides: Partial<ModelSettingsTransport> = {}) {
  const made = makeTransport(overrides);
  render(SavingsPage, { props: { transport: made.transport } });
  return made;
}

describe("SavingsPage", () => {
  it("renders every lever with its switch state and explanation, and the knobs", async () => {
    page();
    expect((await screen.findByRole("checkbox", { name: /Outline reads/ }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: /Recall deltas/ }) as HTMLInputElement).checked).toBe(false);
    expect(screen.getByText("Open a long source file with its outline; ranged reads load the exact lines.")).toBeTruthy();
    expect((screen.getByLabelText("Archive after") as HTMLInputElement).value).toBe("2400");
    expect((screen.getByLabelText("Nudge at fill %") as HTMLInputElement).value).toBe("80");
    expect((screen.getByLabelText("Extra commands") as HTMLInputElement).value).toBe("cargo test, npm run build");
  });

  it("toggles a lever and saves the optimizer section", async () => {
    const made = page();
    await screen.findByRole("checkbox", { name: /Archive big results/ });
    await fireEvent.click(screen.getByRole("checkbox", { name: /Archive big results/ }));
    await fireEvent.click(screen.getByRole("button", { name: "Save levers" }));
    await waitFor(() => expect(made.transport.save).toHaveBeenCalledWith("optimizer", {
      delta_reads: true,
      compress_output: true,
      archive: false,
      compaction_survival: true,
      quality_score: true,
      lean_output_nudge: true,
      recall_delta: false,
      idle_compaction: true,
      archive_min_chars: 2400,
      compress_min_chars: 3000,
      lean_output_fill_pct: 80,
      nudge_cooldown_requests: 4,
      compress_extra_commands: ["cargo test", "npm run build"],
    }));
    expect(await screen.findByText("Applies after reopening the project.")).toBeTruthy();
  });

  it("shows the idle-compaction switch checked by default, and saving a toggle-off carries idle_compaction: false", async () => {
    const made = page();
    const idle = await screen.findByRole("checkbox", { name: /Idle compaction/ });
    expect((idle as HTMLInputElement).checked).toBe(true);
    await fireEvent.click(idle);
    await fireEvent.click(screen.getByRole("button", { name: "Save levers" }));
    await waitFor(() => expect(made.transport.save).toHaveBeenCalledWith("optimizer", {
      delta_reads: true,
      compress_output: true,
      archive: true,
      compaction_survival: true,
      quality_score: true,
      lean_output_nudge: true,
      recall_delta: false,
      idle_compaction: false,
      archive_min_chars: 2400,
      compress_min_chars: 3000,
      lean_output_fill_pct: 80,
      nudge_cooldown_requests: 4,
      compress_extra_commands: ["cargo test", "npm run build"],
    }));
  });

  it("saves edited knobs with the extra commands split on commas", async () => {
    const made = page();
    await screen.findByLabelText("Nudge at fill %");
    await fireEvent.input(screen.getByLabelText("Nudge at fill %"), { target: { value: "75" } });
    await fireEvent.input(screen.getByLabelText("Extra commands"), { target: { value: "cargo clippy, npm test" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save levers" }));
    await waitFor(() => expect(made.transport.save).toHaveBeenCalled());
    const payload = made.transport.save.mock.calls[0][1] as Record<string, unknown>;
    expect(payload.lean_output_fill_pct).toBe(75);
    expect(payload.compress_extra_commands).toEqual(["cargo clippy", "npm test"]);
  });

  it("rejects an out-of-range fill percent inline without saving", async () => {
    const made = page();
    await screen.findByLabelText("Nudge at fill %");
    await fireEvent.input(screen.getByLabelText("Nudge at fill %"), { target: { value: "150" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save levers" }));
    expect(screen.getByText("Nudge at fill % must be a whole number from 0 to 100")).toBeTruthy();
    expect(made.transport.save).not.toHaveBeenCalled();
  });

  it("has no pricing table and points to the Endpoints page", async () => {
    page();
    await screen.findByRole("checkbox", { name: /Outline reads/ });
    expect(screen.queryByRole("region", { name: "Per-model pricing" })).toBeNull();
    expect(screen.queryByLabelText("Model for row 1")).toBeNull();
    expect(screen.queryByRole("button", { name: "Save prices" })).toBeNull();
    expect(screen.getByText("Model prices are set with each model on the Endpoints page.")).toBeTruthy();
  });

  it("shows the load error when the snapshot fails", async () => {
    page({ get: vi.fn(async () => Promise.reject("config unreadable")) });
    expect(await screen.findByText("config unreadable")).toBeTruthy();
  });
});