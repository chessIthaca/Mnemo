import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";
import type { ModelSettings, ModelSettingsSection, ModelSettingsTransport } from "../lib/modelSettings";
import MachinePage from "./MachinePage.svelte";

/**
 * MachinePage — the machine-wide build limit (the "machine" section,
 * `[general.machine]`) plus the read-only free-disk lines. The page must show
 * the saved limit, save a new one, and refuse an out-of-range number instead
 * of writing it.
 */

function sampleSettings(machine?: Partial<ModelSettings["machine"]>): ModelSettings {
  return {
    task_models: { summarize: null },
    vision_model: null,
    vision_warning: null,
    embedding: { selected: "hash", status: "keyword_only", options: [] },
    optimizer: {
      delta_reads: true,
      compress_output: true,
      archive: true,
      compaction_survival: true,
      quality_score: true,
      lean_output_nudge: true,
      recall_delta: true,
      idle_compaction: true,
      archive_min_chars: 2400,
      compress_min_chars: 3000,
      lean_output_fill_pct: 80,
      nudge_cooldown_requests: 4,
      compress_extra_commands: [],
    },
    budget: { enabled: false, max_tokens_per_plan: 0, max_escalations_per_plan: 0, max_retries_per_lane: 0 },
    machine: {
      heavy_commands: 2,
      min_heavy_commands: 1,
      max_heavy_commands: 16,
      disks: [{ role: "project", volume: "C:", free_gb: 123 }],
      sccache: { found: false, path: null },
      ...machine,
    },
  };
}

function makeTransport(settings: ModelSettings) {
  const transport: ModelSettingsTransport = {
    get: vi.fn(async () => structuredClone(settings)),
    save: vi.fn(async (_section: ModelSettingsSection, _value: unknown) => undefined),
    install: vi.fn(async () => undefined),
    onEmbedderEvent: vi.fn(async () => () => {}),
    hostStatus: vi.fn(async () => ({
      running: true,
      port: 8080,
      models: ["all-MiniLM-L6-v2", "jina-embeddings-v2-base-code"],
      windows: 1,
    })),
  };
  return transport;
}

function page(settings: ModelSettings = sampleSettings()) {
  const transport = makeTransport(settings);
  render(MachinePage, { props: { transport } });
  return transport;
}

describe("MachinePage", () => {
  it("shows the saved build limit and the free-disk line", async () => {
    page();
    const field = (await screen.findByLabelText("Builds and tests at once")) as HTMLInputElement;
    expect(field.value).toBe("2");
    expect(screen.getByText("Free disk: 123 GB on C:")).toBeTruthy();
    expect(screen.getByText(/Heavy commands \(cargo build\/test\/clippy, npm test\/build\/check\)/)).toBeTruthy();
  });

  it("shows a second line when the config dir is on another volume", async () => {
    page(
      sampleSettings({
        disks: [
          { role: "project", volume: "C:", free_gb: 123 },
          { role: "config", volume: "D:", free_gb: 40 },
        ],
      }),
    );
    expect(await screen.findByText("Free disk: 123 GB on C:")).toBeTruthy();
    expect(screen.getByText("Free disk: 40 GB on D:")).toBeTruthy();
  });

  it("saves a changed limit as the machine section", async () => {
    const transport = page();
    const field = (await screen.findByLabelText("Builds and tests at once")) as HTMLInputElement;
    await fireEvent.input(field, { target: { value: "1" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save limit" }));
    await waitFor(() => expect(transport.save).toHaveBeenCalledWith("machine", { heavy_commands: 1 }));
    expect(await screen.findByText("Applies to the next heavy command in every window.")).toBeTruthy();
  });

  it("refuses an out-of-range limit instead of saving it", async () => {
    const transport = page();
    const field = (await screen.findByLabelText("Builds and tests at once")) as HTMLInputElement;
    await fireEvent.input(field, { target: { value: "99" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save limit" }));
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "Builds and tests at once must be a whole number from 1 to 16",
    );
    expect(transport.save).not.toHaveBeenCalled();
  });

  it("names sccache when the shared build cache was found", async () => {
    page(sampleSettings({ sccache: { found: true, path: "C:\\Users\\me\\.cargo\\bin\\sccache.exe" } }));
    expect(
      await screen.findByText("Shared Rust build cache: sccache (C:\\Users\\me\\.cargo\\bin\\sccache.exe)"),
    ).toBeTruthy();
  });

  it("says how to install sccache when none was found", async () => {
    page();
    expect(
      await screen.findByText("Shared Rust build cache: off; install it with cargo install sccache"),
    ).toBeTruthy();
  });

  it("shows the shared model host's status", async () => {
    page();
    expect(
      await screen.findByText(
        "Shared model host: running on port 8080, serving all-MiniLM-L6-v2, jina-embeddings-v2-base-code, used by 1 window",
      ),
    ).toBeTruthy();
  });
});
