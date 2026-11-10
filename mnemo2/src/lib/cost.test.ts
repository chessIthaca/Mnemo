import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  costPreviewInsights,
  costViewModel,
  createCostTransport,
  createTauriCostTransport,
  dayLabel,
  formatEstimate,
  formatHitRate,
  formatSignedTokens,
  leverExplanation,
  leverLabel,
  seedCostPreview,
  shortModelLabel,
  type CostInsights,
  type CostReport,
  type SavingsReport,
} from "./cost";

// A plain browser page has no host; the transport factory must agree.
vi.mock("./runtime", () => ({ isTauriRuntime: () => false }));

const DAY = 86_400;

/** Day 100 after epoch is 1970-04-11 UTC — a stable, month-crossing anchor. */
const DAY_A = 100 * DAY;
const DAY_B = 101 * DAY;
const DAY_C = 102 * DAY;

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
      model: "glm-5.3-flash",
      input_tokens: 200_000,
      cached_tokens: 100_000,
      output_tokens: 10_000,
      cost_micros: 104_000,
    },
  ],
  unpriced_models: [],
  per_day: [
    { day: DAY_A, cost_micros: 1_500_000 },
    { day: DAY_B, cost_micros: 604_000 },
  ],
  ...overrides,
});

const savings = (overrides: Partial<SavingsReport> = {}): SavingsReport => ({
  tokens_saved: 1_160_000,
  estimated_saved_micros: 180_000,
  levers: [
    { kind: "compression", events: 40, tokens_saved: 1_200_000, estimated_saved_micros: 180_000, estimated_cost_micros: null },
    { kind: "archive_expand", events: 2, tokens_saved: -40_000, estimated_saved_micros: 0, estimated_cost_micros: 20_000 },
  ],
  cache: { prompt_tokens: 2_000_000, cached_tokens: 1_540_000, hit_rate: 0.77, estimated_saved_micros: 120_000 },
  per_day: [
    { day: DAY_A, tokens_saved: 1_000_000 },
    { day: DAY_C, tokens_saved: 90_000 },
  ],
  traffic_micros: 12_500_000,
  traffic_since: DAY_A,
  ...overrides,
});

describe("lever names", () => {
  it("maps every ledger kind to a friendly name and a one-line explanation", () => {
    expect(leverLabel("compression")).toBe("Output compression");
    expect(leverLabel("old_result_trim")).toBe("Old results trimmed");
    expect(leverLabel("idle_compaction")).toBe("Idle compaction");
    // Dropped from the table: a checkpoint still prettifies through the
    // fallback rather than rendering as its raw id.
    expect(leverLabel("compaction_checkpoint")).toBe("Compaction checkpoint");
    expect(leverExplanation("archive")).toBe(
      "Oversized tool results are archived in full; only a short preview stays in context.",
    );
  });

  it("prettifies an unknown kind instead of showing its raw id", () => {
    expect(leverLabel("some_new_lever")).toBe("Some new lever");
    expect(leverExplanation("some_new_lever")).toBe("An optimization lever the engine recorded.");
  });
});

describe("cost formatting", () => {
  it("shortens model ids by dropping provider prefixes", () => {
    expect(shortModelLabel("openrouter/anthropic/claude-opus-5-5")).toBe("claude-opus-5-5");
    expect(shortModelLabel("glm-5.3-flash")).toBe("glm-5.3-flash");
  });

  it("estimates carry the tilde, unknown prices show a dash", () => {
    expect(formatEstimate(420_000)).toBe("≈ $0.42");
    expect(formatEstimate(null)).toBe("—");
  });

  it("keeps the sign on savings, unlike the clamping token formatter", () => {
    expect(formatSignedTokens(-40_000)).toBe("-40,000");
    expect(formatSignedTokens(40_000)).toBe("40,000");
  });

  it("renders a hit rate as a whole percentage", () => {
    expect(formatHitRate(0.77)).toBe("77%");
    expect(formatHitRate(0)).toBe("0%");
    expect(formatHitRate(1)).toBe("100%");
    expect(formatHitRate(1.5)).toBe("100%");
  });

  it("labels a UTC-day bucket in UTC so the label matches the bucket", () => {
    expect(dayLabel(DAY_A)).toBe("Apr 11");
  });
});

describe("costViewModel", () => {
  it("builds the headline from the totals", () => {
    const view = costViewModel(cost(), savings());
    expect(view.spendLabel).toBe("$2.10");
    expect(view.tokensLabel).toBe("1,200,000 in · 800,000 cached · 90,000 out");
    expect(view.partial).toBe(false);
    expect(view.empty).toBe(false);
  });

  it("spends by model with priced rows first, unpriced rows last", () => {
    const report = cost({
      models: [
        ...cost().models,
        { model: "ollama/qwen3:32b", input_tokens: 240_000, cached_tokens: 60_000, output_tokens: 30_000, cost_micros: null },
      ],
      unpriced_models: ["ollama/qwen3:32b"],
    });
    const view = costViewModel(report, savings());
    expect(view.models.map((row) => row.id)).toEqual([
      "anthropic/claude-opus-5-5",
      "glm-5.3-flash",
      "ollama/qwen3:32b",
    ]);
    expect(view.models[0]).toMatchObject({ label: "claude-opus-5-5", costLabel: "$2.00", barFraction: 1 });
    expect(view.models[2]).toMatchObject({ costLabel: "no price", costMicros: null });
    expect(view.partial).toBe(true);
    expect(view.unpricedLabel).toBe("qwen3:32b");
  });

  it("gives every lever a friendly name, an explanation and a dollar figure", () => {
    const view = costViewModel(cost(), savings());
    expect(view.levers[0]).toMatchObject({
      kind: "compression",
      label: "Output compression",
      metaLabel: "1,200,000 tokens · 40 events",
      costLabel: "≈ $0.18",
    });
    expect(view.savingsTokensLabel).toBe("1,160,000");
    expect(view.savingsCostLabel).toBe("≈ $0.18");
  });

  it("a re-adding lever shows what it cost", () => {
    const view = costViewModel(cost(), savings());
    // The expansion sinks to the bottom, shows the tokens it re-added and the
    // dollars it cost.
    expect(view.levers[1]).toMatchObject({
      kind: "archive_expand",
      readded: true,
      metaLabel: "+40,000 tokens re-added · 2 events",
      costLabel: "costs ≈ $0.02",
    });
  });

  it("a re-adding lever with no price shows no dollars", () => {
    // No price anywhere: the wire carries a null cost, so the row keeps its
    // token count and stays silent about dollars.
    const unpriced = savings({
      levers: [
        { kind: "compression", events: 40, tokens_saved: 1_200_000, estimated_saved_micros: null, estimated_cost_micros: null },
        { kind: "archive_expand", events: 2, tokens_saved: -40_000, estimated_saved_micros: null, estimated_cost_micros: null },
      ],
    });
    const view = costViewModel(cost(), unpriced);
    expect(view.levers[1]).toMatchObject({
      readded: true,
      metaLabel: "+40,000 tokens re-added · 2 events",
      costLabel: "",
    });
  });

  it("the savings notes name the traffic and the method", () => {
    const view = costViewModel(cost(), savings());
    // The tilde comes from the template, not from formatEstimate.
    expect(view.savingsBasisLabel).toBe(
      "On ≈ $12.50 of model traffic since Apr 11. The spend above counts Mnemo2's own ledger.",
    );
    expect(view.savingsMethodLabel).toBe(
      "Estimates: credited against the 100,000-character result cap, at each session's model rates, counting up to 15 later re-sends.",
    );
  });

  it("no traffic note without a price", () => {
    const view = costViewModel(cost(), savings({ traffic_micros: null, traffic_since: null }));
    expect(view.savingsBasisLabel).toBe("");
    // The method note is always there: the figure is never bare.
    expect(view.savingsMethodLabel).not.toBe("");
  });

  it("reports the prompt cache hit rate and its dollars", () => {
    const view = costViewModel(cost(), savings());
    expect(view.cacheActive).toBe(true);
    expect(view.cacheHitRateLabel).toBe("77%");
    expect(view.cacheDetailLabel).toBe("1,540,000 of 2,000,000 prompt tokens served from cache");
    expect(view.cacheSavedLabel).toBe("≈ $0.12");
  });

  it("merges both day series into aligned columns with month-aware labels", () => {
    const view = costViewModel(cost(), savings());
    // The union of {A, B} and {A, C}, ascending, each series' own fractions.
    expect(view.days.map((day) => day.label)).toEqual(["Apr 11", "Apr 12", "Apr 13"]);
    expect(view.days.map((day) => day.shortLabel)).toEqual(["Apr 11", "12", "13"]);
    expect(view.days[0]).toMatchObject({ costFraction: 1, tokensFraction: 1, tokensSaved: 1_000_000 });
    expect(view.days[1]).toMatchObject({ costMicros: 604_000, tokensSaved: 0, tokensFraction: 0 });
    expect(view.days[2]).toMatchObject({ costMicros: 0, tokensSaved: 90_000, costFraction: 0 });
    expect(view.daysHaveSpend).toBe(true);
    expect(view.daysHaveSavings).toBe(true);
  });

  it("clamps the day series to the last 14 buckets", () => {
    const report = cost({
      per_day: Array.from({ length: 20 }, (_, index) => ({
        day: (80 + index) * DAY,
        cost_micros: 100,
      })),
    });
    const view = costViewModel(report, savings({ per_day: [] }));
    expect(view.days).toHaveLength(14);
    expect(view.days[0]!.day).toBe(86 * DAY);
    expect(view.days[13]!.day).toBe(99 * DAY);
  });

  it("marks zero reports as empty", () => {
    const empty = costViewModel(cost({ total: { input_tokens: 0, cached_tokens: 0, output_tokens: 0, cost_micros: 0 }, models: [], per_day: [] }), savings({ tokens_saved: 0, estimated_saved_micros: null, levers: [], per_day: [], cache: { prompt_tokens: 0, cached_tokens: 0, hit_rate: 0, estimated_saved_micros: null } }));
    expect(empty.empty).toBe(true);
  });

  it("is not empty when only savings exist", () => {
    const view = costViewModel(cost({ total: { input_tokens: 0, cached_tokens: 0, output_tokens: 0, cost_micros: 0 }, models: [], per_day: [] }), savings());
    expect(view.empty).toBe(false);
  });
});

describe("cost transport", () => {
  it("returns null outside the host and a transport inside it", () => {
    expect(createCostTransport()).toBeNull();
    const withBindings = createCostTransport({ invoke: (async () => cost()) as never });
    expect(withBindings).not.toBeNull();
  });

  it("invokes the two insights commands", async () => {
    const invoke = vi.fn(async (command: string) => (command === "insights_cost" ? cost() : savings()));
    const transport = createTauriCostTransport({ invoke: invoke as never });
    await expect(transport.cost()).resolves.toEqual(cost());
    await expect(transport.savings()).resolves.toEqual(savings());
    expect(invoke).toHaveBeenNthCalledWith(1, "insights_cost");
    expect(invoke).toHaveBeenNthCalledWith(2, "insights_savings");
  });
});

describe("preview seed", () => {
  beforeEach(() => {
    seedCostPreview(null);
  });

  it("holds the seeded reports until cleared", () => {
    expect(costPreviewInsights()).toBeNull();
    const insights: CostInsights = { cost: cost(), savings: savings() };
    seedCostPreview(insights);
    expect(costPreviewInsights()).toBe(insights);
    seedCostPreview(null);
    expect(costPreviewInsights()).toBeNull();
  });
});