import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";
import type { ModelSettings, ModelSettingsSection, ModelSettingsTransport } from "../lib/modelSettings";
import { emptyModel, type EndpointDetailDto } from "../lib/settingsEngine";
import SavingsPage from "./SavingsPage.svelte";

/**
 * SavingsPage — the optimizer levers with their knobs and the per-model
 * pricing table, each section saving on its own button.
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
      archive_min_chars: 2400,
      compress_min_chars: 3000,
      lean_output_fill_pct: 80,
      nudge_cooldown_requests: 4,
      compress_extra_commands: ["cargo test", "npm run build"],
    },
    pricing: [
      { model: "glm-4.6-flash", input_per_1m: 0.12, output_per_1m: 0.39, cached_per_1m: 0.012 },
      { model: "gpt-5.2", input_per_1m: 1.25, output_per_1m: 10, cached_per_1m: 0.125 },
      { model: "qwen3-30b-a3b", input_per_1m: 0.05, output_per_1m: 0.2, cached_per_1m: 0 },
    ],
    budget: { enabled: false, max_tokens_per_plan: 0, max_escalations_per_plan: 0, max_retries_per_lane: 0 },
  };
}

function sampleEndpoints(): EndpointDetailDto[] {
  return [
    {
      name: "openrouter",
      kind: "openrouter",
      base_url: "https://openrouter.ai/api/v1/",
      has_api_key: true,
      max_context: null,
      max_output_tokens: null,
      multimodal: false,
      supports_reasoning_effort: true,
      reasoning_effort: null,
      supports_strict_schema: null,
      workspace_id: null,
      temperature: null,
      models: [emptyModel("glm-4.6-flash"), emptyModel("gpt-5.2")],
    },
  ];
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
  render(SavingsPage, { props: { transport: made.transport, endpoints: sampleEndpoints() } });
  return made;
}

describe("SavingsPage", () => {
  it("renders every lever with its switch state and explanation, the knobs and the pricing table", async () => {
    page();
    expect((await screen.findByRole("checkbox", { name: /Delta reads/ }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: /Recall deltas/ }) as HTMLInputElement).checked).toBe(false);
    expect(screen.getByText("Re-read file signatures and diffs instead of full file contents.")).toBeTruthy();
    expect((screen.getByLabelText("Archive after") as HTMLInputElement).value).toBe("2400");
    expect((screen.getByLabelText("Nudge at fill %") as HTMLInputElement).value).toBe("80");
    expect((screen.getByLabelText("Extra commands") as HTMLInputElement).value).toBe("cargo test, npm run build");
    expect((screen.getByLabelText("Model for row 1") as HTMLInputElement).value).toBe("glm-4.6-flash");
    expect((screen.getByLabelText("Input price for row 1") as HTMLInputElement).value).toBe("0.12");
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
      archive_min_chars: 2400,
      compress_min_chars: 3000,
      lean_output_fill_pct: 80,
      nudge_cooldown_requests: 4,
      compress_extra_commands: ["cargo test", "npm run build"],
    }));
    expect(await screen.findByText("Applies after reopening the project.")).toBeTruthy();
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

  it("edits prices and saves the pricing section", async () => {
    const made = page();
    await screen.findByLabelText("Input price for row 1");
    await fireEvent.input(screen.getByLabelText("Input price for row 1"), { target: { value: "0.15" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save prices" }));
    await waitFor(() =>
      expect(made.transport.save).toHaveBeenCalledWith("pricing", [
        { model: "glm-4.6-flash", input_per_1m: 0.15, output_per_1m: 0.39, cached_per_1m: 0.012 },
        { model: "gpt-5.2", input_per_1m: 1.25, output_per_1m: 10, cached_per_1m: 0.125 },
        { model: "qwen3-30b-a3b", input_per_1m: 0.05, output_per_1m: 0.2, cached_per_1m: 0 },
      ]),
    );
    expect(await screen.findByText("Applies after reopening the project.")).toBeTruthy();
  });

  it("adds and removes pricing rows", async () => {
    const made = page();
    await screen.findByLabelText("Model for row 1");
    await fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    await fireEvent.input(screen.getByLabelText("Model for row 4"), { target: { value: "fresh-model" } });
    await fireEvent.click(screen.getByRole("button", { name: "Remove row 2" }));
    await fireEvent.click(screen.getByRole("button", { name: "Save prices" }));
    await waitFor(() =>
      expect(made.transport.save).toHaveBeenCalledWith("pricing", [
        { model: "glm-4.6-flash", input_per_1m: 0.12, output_per_1m: 0.39, cached_per_1m: 0.012 },
        { model: "qwen3-30b-a3b", input_per_1m: 0.05, output_per_1m: 0.2, cached_per_1m: 0 },
        { model: "fresh-model", input_per_1m: 0, output_per_1m: 0, cached_per_1m: 0 },
      ]),
    );
  });

  it("skips blank-model rows on save", async () => {
    const made = page();
    await screen.findByLabelText("Model for row 1");
    await fireEvent.input(screen.getByLabelText("Model for row 2"), { target: { value: "" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save prices" }));
    await waitFor(() => expect(made.transport.save).toHaveBeenCalled());
    const rows = made.transport.save.mock.calls[0][1] as { model: string }[];
    expect(rows.map((row) => row.model)).toEqual(["glm-4.6-flash", "qwen3-30b-a3b"]);
  });

  it("rejects a non-numeric price inline", async () => {
    const made = page();
    await screen.findByLabelText("Output price for row 1");
    await fireEvent.input(screen.getByLabelText("Output price for row 1"), { target: { value: "cheap" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save prices" }));
    expect(screen.getByText("Output price for glm-4.6-flash must be a number")).toBeTruthy();
    expect(made.transport.save).not.toHaveBeenCalled();
  });

  it("shows the load error when the snapshot fails", async () => {
    page({ get: vi.fn(async () => Promise.reject("config unreadable")) });
    expect(await screen.findByText("config unreadable")).toBeTruthy();
  });
});