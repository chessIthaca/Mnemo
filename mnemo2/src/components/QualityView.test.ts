import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import QualityView from "./QualityView.svelte";
import type { EditOutcomes, QualityReport, QualityRow, QualityTransport } from "../lib/quality";

const mocks = vi.hoisted(() => ({
  transport: null as QualityTransport | null,
}));

// QualityView builds its own transport on mount; the tests plant one instead.
vi.mock("../lib/quality", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/quality")>();
  return { ...original, createQualityTransport: () => mocks.transport };
});

function row(key: string, overrides: Partial<QualityRow> = {}): QualityRow {
  return {
    key,
    merged: 4,
    firstPass: 0.5,
    avgFixRounds: 0.5,
    avgFindings: 1.5,
    highFindings: 0,
    reverted: 0,
    bugfixFollowups: 0,
    costPerMergedMicros: 2_400_000,
    ...overrides,
  };
}

function report(overrides: Partial<QualityReport> = {}): QualityReport {
  return {
    windowDays: 30,
    generatedMs: 1_800_000_000_000,
    byTier: [row("high", { merged: 6, firstPass: 0.5, avgFixRounds: 0.7, avgFindings: 2.5 })],
    byModel: [row("openai/gpt-5", { merged: 4, firstPass: 0.25, avgFixRounds: 1.25 })],
    categories: [],
    missingMergeCommit: 0,
    edits: { byModel: [], byWorker: [], topFiles: [], skipped: 0 },
    ...overrides,
  };
}

/** The cells of one table's body rows, key column first. */
function cells(table: HTMLElement): string[][] {
  return Array.from(table.querySelectorAll("tbody tr")).map((line) =>
    Array.from(line.querySelectorAll("th, td")).map((cell) => cell.textContent?.trim() ?? ""),
  );
}

beforeEach(() => {
  mocks.transport = null;
});

describe("QualityView", () => {
  it("renders the by-tier and by-model tables from the report", async () => {
    mocks.transport = { report: async () => report({ categories: [["missing regression test", 3]] }) };
    render(QualityView);

    const tier = await screen.findByRole("table", { name: "By tier" });
    const model = screen.getByRole("table", { name: "By model" });
    expect(screen.getByRole("heading", { name: "By tier" })).toBeTruthy();

    // Header cells name the metrics the plan asks for, in order.
    expect(within(tier).getAllByRole("columnheader").map((cell) => cell.textContent?.trim())).toEqual([
      "Tier",
      "Merged",
      "First pass",
      "Fix rounds",
      "Findings",
      "Reverted",
      "Bug fix followed",
      "Cost / merged",
    ]);

    // One row per key, with the share as a percent, means to one decimal and
    // the cost through the Cost tab's own micros formatter.
    expect(cells(tier)).toEqual([["high", "6", "50%", "0.7", "2.5", "0", "0", "$2.40"]]);
    expect(cells(model)).toEqual([["openai/gpt-5", "4", "25%", "1.3", "1.5", "0", "0", "$2.40"]]);

    // The recurring finding categories list their distinct workers.
    expect(await screen.findByRole("heading", { name: "Recurring findings" })).toBeTruthy();
    expect(screen.getByText("missing regression test")).toBeTruthy();
    expect(screen.getByTitle(/Distinct workers/).textContent).toBe("3");
  });

  it("says so when the window has no merges", async () => {
    mocks.transport = { report: async () => report({ byTier: [], byModel: [], categories: [] }) };
    render(QualityView);

    expect(await screen.findByText("No merged work in this window yet.")).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
    expect(screen.queryByRole("heading", { name: "By tier" })).toBeNull();
    expect(screen.queryByRole("heading", { name: "Recurring findings" })).toBeNull();
  });

  it("shows edit attempts and failures by cause, model, worker and file", async () => {
    const edits: EditOutcomes = {
      byModel: [
        {
          key: "glm-5.3",
          attempts: 40,
          ok: 24,
          shape: 7,
          receiptDrift: 2,
          artifact: 3,
          citeRange: 2,
          mismatch: 1,
          blocked: 1,
          other: 0,
        },
      ],
      byWorker: [
        {
          key: "simFactories01",
          attempts: 40,
          ok: 24,
          shape: 7,
          receiptDrift: 2,
          artifact: 3,
          citeRange: 2,
          mismatch: 1,
          blocked: 1,
          other: 0,
        },
      ],
      topFiles: [
        {
          path: "crates/fg_sim/src/layout.rs",
          attempts: 12,
          failures: 5,
          shape: 2,
          receiptDrift: 1,
          artifact: 1,
          citeRange: 1,
          mismatch: 0,
          blocked: 0,
          other: 0,
        },
      ],
      skipped: 0,
    };
    mocks.transport = { report: async () => report({ edits }) };
    render(QualityView);

    expect(await screen.findByRole("heading", { name: "Edits" })).toBeTruthy();
    const models = await screen.findByRole("table", { name: "Edits by model" });
    expect(
      within(models).getAllByRole("columnheader").map((cell) => cell.textContent?.trim()),
    ).toEqual([
      "Model",
      "Attempts",
      "Failure rate",
      "Shape",
      "Receipt drift",
      "Artifact",
      "Cite range",
      "Mismatch",
      "Blocked",
      "Other",
    ]);
    expect(cells(models)).toEqual([
      ["glm-5.3", "40", "40%", "7", "2", "3", "2", "1", "1", "0"],
    ]);

    // The per-worker table is expandable but in the DOM.
    expect(cells(screen.getByRole("table", { name: "Edits by worker" }))).toEqual([
      ["simFactories01", "40", "40%", "7", "2", "3", "2", "1", "1", "0"],
    ]);

    expect(cells(screen.getByRole("table", { name: "Files with the most edit failures" }))).toEqual([
      ["crates/fg_sim/src/layout.rs", "12", "5", "2", "1", "1", "1", "0", "0", "0"],
    ]);
  });

  it("says so when the window has no edit calls", async () => {
    mocks.transport = { report: async () => report() };
    render(QualityView);

    expect(await screen.findByText("No edit calls in this window.")).toBeTruthy();
    expect(screen.queryByRole("table", { name: "Edits by model" })).toBeNull();
  });

  it("notes the older tasks without a merge commit only when there are some", async () => {
    mocks.transport = { report: async () => report({ missingMergeCommit: 3 }) };
    const { unmount } = render(QualityView);
    expect(await screen.findByText(/3 older tasks have no merge commit recorded/)).toBeTruthy();
    unmount();

    mocks.transport = { report: async () => report({ missingMergeCommit: 0 }) };
    render(QualityView);
    await screen.findByRole("table", { name: "By tier" });
    expect(screen.queryByText(/older tasks have no merge commit recorded/)).toBeNull();
  });

  it("re-requests the report when the window changes", async () => {
    const reportMock = vi.fn(async (days: number) => report({ windowDays: days }));
    mocks.transport = { report: reportMock };
    render(QualityView);

    await waitFor(() => expect(reportMock).toHaveBeenCalledWith(30));
    // The selector opens on the report's own default window.
    expect((screen.getByRole("combobox", { name: "Window" }) as HTMLSelectElement).value).toBe("30");

    await fireEvent.change(screen.getByRole("combobox", { name: "Window" }), {
      target: { value: "90" },
    });
    await waitFor(() => expect(reportMock).toHaveBeenLastCalledWith(90));
    expect(reportMock.mock.calls.map((call) => call[0])).toEqual([30, 90]);
  });
});
