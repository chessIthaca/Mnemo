import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import CostView from "./CostView.svelte";
import {
  seedCostPreview,
  type CostReport,
  type CostTransport,
  type SavingsReport,
} from "../lib/cost";

const mocks = vi.hoisted(() => ({
  transport: null as CostTransport | null,
}));

// CostView builds its own transport on mount; tests swap it, SettingsDialog-style.
vi.mock("../lib/cost", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/cost")>();
  return { ...original, createCostTransport: () => mocks.transport };
});

const DAY = 86_400;

const cost = (overrides: Partial<CostReport> = {}): CostReport => ({
  total: { input_tokens: 1_200_000, cached_tokens: 800_000, output_tokens: 90_000, cost_micros: 2_104_000 },
  models: [
    {
      model: "anthropic/claude-opus-5-5",
      input_tokens: 1_000_000,
      cached_tokens: 700_000,
      output_tokens: 80_000,
      cost_micros: 2_000_000,
    },
    {
      model: "ollama/qwen3:32b",
      input_tokens: 200_000,
      cached_tokens: 100_000,
      output_tokens: 10_000,
      cost_micros: null,
    },
  ],
  unpriced_models: ["ollama/qwen3:32b"],
  per_day: [
    { day: 100 * DAY, cost_micros: 1_500_000 },
    { day: 101 * DAY, cost_micros: 604_000 },
  ],
  ...overrides,
});

const savings = (overrides: Partial<SavingsReport> = {}): SavingsReport => ({
  tokens_saved: 1_080_000,
  estimated_saved_micros: 180_000,
  levers: [
    { kind: "delta_read", events: 40, tokens_saved: 1_200_000, estimated_saved_micros: 180_000, estimated_cost_micros: null },
    { kind: "archive_expand", events: 6, tokens_saved: -120_000, estimated_saved_micros: 0, estimated_cost_micros: 20_000 },
  ],
  cache: { prompt_tokens: 2_000_000, cached_tokens: 1_540_000, hit_rate: 0.77, estimated_saved_micros: 120_000 },
  per_day: [{ day: 100 * DAY, tokens_saved: 1_000_000 }],
  traffic_micros: 12_500_000,
  traffic_since: 100 * DAY,
  ...overrides,
});

/** A stub transport answering both commands with the sample reports. */
function makeTransport(
  costReport: CostReport | Error = cost(),
  savingsReport: SavingsReport | Error = savings(),
): CostTransport {
  const answer = <T,>(report: T | Error): Promise<T> =>
    report instanceof Error ? Promise.reject(report) : Promise.resolve(report);
  const transport = {
    cost: vi.fn(() => answer(costReport)),
    savings: vi.fn(() => answer(savingsReport)),
  };
  mocks.transport = transport;
  return transport;
}

beforeEach(() => {
  mocks.transport = null;
  seedCostPreview(null);
});

afterEach(() => {
  vi.useRealTimers();
});

describe("CostView", () => {
  it("renders the headline, the models, the savings and the cache once the transport answers", async () => {
    makeTransport();
    render(CostView);

    // Headline spend and the token split.
    expect(await screen.findByText("$2.10")).toBeTruthy();
    expect(screen.getByText("1,200,000 in · 800,000 cached · 90,000 out")).toBeTruthy();

    // Partial estimate note names the unpriced model.
    expect(screen.getByText(/Partial estimate — no price for qwen3:32b/)).toBeTruthy();

    // Spend by model: priced dollars and the "no price" cell.
    expect(screen.getByText("$2.00")).toBeTruthy();
    expect(screen.getByText("no price")).toBeTruthy();

    // Savings with a friendly lever name, and the cache hit rate.
    expect(screen.getByText("Delta reads")).toBeTruthy();
    expect(screen.getByText("77%")).toBeTruthy();

    // The expansion is a cost row: re-added tokens and what they cost.
    const expansions = screen.getByText("Archive expansions").closest(".lever-row") as HTMLElement;
    expect(within(expansions).getByText("+120,000 tokens re-added · 6 events")).toBeTruthy();
    expect(within(expansions).getByText("costs ≈ $0.02")).toBeTruthy();

    // The savings card says how the estimate is made, and against what.
    expect(screen.getByText(/credited against the 100,000-character result cap/)).toBeTruthy();
    expect(screen.getByText(/On ≈ \$12\.50 of model traffic since Apr 11\./)).toBeTruthy();

    // The day charts carry month-aware labels.
    expect(screen.getAllByText("Apr 11").length).toBeGreaterThan(0);
  });

  it("shows the empty state when nothing has run yet", async () => {
    makeTransport(
      cost({ total: { input_tokens: 0, cached_tokens: 0, output_tokens: 0, cost_micros: 0 }, models: [], per_day: [] }),
      savings({ tokens_saved: 0, estimated_saved_micros: null, levers: [], per_day: [], cache: { prompt_tokens: 0, cached_tokens: 0, hit_rate: 0, estimated_saved_micros: null } }),
    );
    render(CostView);

    expect(await screen.findByText(/Nothing recorded yet/)).toBeTruthy();
    expect(screen.queryByText("Spend by model")).toBeNull();
  });

  it("shows an inline error with a retry, never a toast", async () => {
    const transport = makeTransport(new Error("no project open"));
    render(CostView);

    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toBe("no project open");

    // The retry answers with the real reports and clears the error.
    transport.cost = vi.fn(async () => cost());
    transport.savings = vi.fn(async () => savings());
    await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(screen.getByText("$2.10")).toBeTruthy());
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("keeps the last reports and surfaces a failed refresh as a status note", async () => {
    vi.useFakeTimers();
    try {
      const transport = makeTransport();
      render(CostView);
      await vi.advanceTimersByTimeAsync(1);
      expect(screen.getByText("$2.10")).toBeTruthy();

      // The next 30 s poll fails: the stale reports stay, the error rides inline.
      transport.cost = vi.fn(() => Promise.reject(new Error("the ledger is busy")));
      await vi.advanceTimersByTimeAsync(30_000);
      expect(screen.getByText("$2.10")).toBeTruthy();
      expect(screen.getByRole("status").textContent).toBe("the ledger is busy");
      expect(screen.queryByRole("alert")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("refreshes every 30 seconds while the tab is visible and stops on unmount", async () => {
    vi.useFakeTimers();
    try {
      const transport = makeTransport();
      const { unmount } = render(CostView);
      await vi.advanceTimersByTimeAsync(1);
      expect(transport.cost).toHaveBeenCalledTimes(1);

      await vi.advanceTimersByTimeAsync(30_000);
      expect(transport.cost).toHaveBeenCalledTimes(2);

      unmount();
      await vi.advanceTimersByTimeAsync(60_000);
      expect(transport.cost).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("notes the desktop app outside the host when no sample is seeded", () => {
    render(CostView);
    expect(screen.getByText("Cost insights are available in the desktop app.")).toBeTruthy();
    expect(screen.queryByText("Spend by model")).toBeNull();
  });

  it("renders the seeded sample outside the host, as the preview page does", () => {
    seedCostPreview({ cost: cost(), savings: savings() });
    render(CostView);
    // The seeded reports render without any transport at all.
    expect(screen.getByText("$2.10")).toBeTruthy();
    expect(screen.getByText("Delta reads")).toBeTruthy();
  });
});