import { describe, expect, it } from "vitest";
import {
  attributionLabel,
  bucketCostLabel,
  cacheReusePct,
  costBucketLabel,
  costBucketStart,
  costBucketTitle,
  costBucketUnit,
  costChart,
  timingChart,
  timingValues,
  tokenChart,
  tokenValues,
  TIMING_SEGMENTS,
} from "./traceCharts";
import type { TraceAttribution, TraceRow } from "./trace";
const MINUTE = 60_000;
/** 2026-11-02T14:00:00Z — a whole minute, hour, and UTC day. */
const BASE = Date.UTC(2026, 10, 2, 14, 0, 0);

/** An attribution with the lead's defaults. */
function attribution(overrides: Partial<TraceAttribution> = {}): TraceAttribution {
  return {
    actor_kind: "lead",
    agent_id: "agent-lead",
    session_id: "session-1",
    agent_name: "Lead",
    worker_id: null,
    plan_id: null,
    plan_step: null,
    turn_id: "turn-1",
    purpose: "turn",
    ...overrides,
  };
}

/** One complete trace row with sane defaults; override only what a test needs. */
function row(overrides: Partial<TraceRow> = {}): TraceRow {
  return {
    id: 1,
    run_id: "run-1",
    ts_ms: BASE,
    model: "kimi-k3.1-pro",
    base_url: "https://api.example.com",
    provider: "zai",
    http_status: 200,
    usage: { prompt: 1000, completion: 200, reasoning: 50, cached: 400 },
    finish_reason: "stop",
    is_complete: true,
    version: 1,
    ttft_ms: 900,
    generation_ms: 4000,
    reasoning_ms: 600,
    connect_ms: 500,
    tools_ms: 800,
    prep_ms: 1000,
    compact_ms: 300,
    backoff_ms: 200,
    stall_ms: 400,
    error: null,
    cancelled: false,
    response_truncated: false,
    response_evicted: false,
    guard_cut: null,
    attribution: attribution(),
    cost_micros: 1000,
    priced: true,
    retained: true,
    ...overrides,
  };
}

describe("timingChart", () => {
  it("applies the phase-attribution spec without double counting", () => {
    const [column] = timingChart([row()]).columns;

    // Setup is prep_ms and CONTAINS compaction; compaction is a hatch, not a slice.
    expect(column.setupMs).toBe(1000);
    expect(column.compactionMs).toBe(300);
    // First token follows the headers: ttft_ms − connect_ms.
    expect(column.connectMs).toBe(500);
    expect(column.firstTokenMs).toBe(400);
    // Generation is generation_ms and CONTAINS stalls.
    expect(column.generationMs).toBe(4000);
    expect(column.stallMs).toBe(400);

    // The stack is the six disjoint phases only.
    expect(column.stackMs).toBe(200 + 1000 + 500 + 400 + 4000 + 800);
    const values = timingValues(column);
    expect(Object.keys(values)).toHaveLength(6);
    expect(Object.values(values).reduce((sum, value) => sum + value, 0)).toBe(column.stackMs);
    expect(column.stackMs).not.toBe(column.stackMs + column.compactionMs + column.stallMs);
  });

  it("saturates first-token and containment maths on inconsistent reports", () => {
    const [column] = timingChart([
      row({ ttft_ms: 200, connect_ms: 500, compact_ms: 5000, prep_ms: 1000, stall_ms: 9000, generation_ms: 500 }),
    ]).columns;

    expect(column.firstTokenMs).toBe(0);
    expect(column.compactionMs).toBe(1000);
    expect(column.stallMs).toBe(500);
    expect(column.stackMs).toBe(200 + 1000 + 500 + 0 + 500 + 800);
  });

  it("reads missing timings as zero and reports hasData honestly", () => {
    const chart = timingChart([
      row({
        ttft_ms: null,
        generation_ms: null,
        connect_ms: null,
        tools_ms: null,
        prep_ms: null,
        compact_ms: null,
        backoff_ms: null,
        stall_ms: null,
      }),
    ]);
    expect(chart.columns[0].stackMs).toBe(0);
    expect(chart.hasData).toBe(false);
    expect(chart.maxVisibleMs).toBe(0);
  });

  it("sorts columns oldest first and carries the row status", () => {
    const chart = timingChart([
      row({ id: 2, ts_ms: BASE + MINUTE, error: "boom", http_status: null, finish_reason: null }),
      row({ id: 1, ts_ms: BASE, cancelled: true }),
    ]);
    expect(chart.columns.map((column) => column.id)).toEqual([1, 2]);
    expect(chart.columns[0].status).toEqual({ label: "cancelled", tone: "stopped" });
    expect(chart.columns[1].status).toEqual({ label: "failed", tone: "error" });
  });

  it("drops hidden segments from the height driver and rescales columns", () => {
    const chart = timingChart(
      [row({ id: 1 }), row({ id: 2, prep_ms: 1000, compact_ms: 0, backoff_ms: 0, tools_ms: 0 })],
      new Set(["setup"]),
    );
    // Column 1 keeps retry+connect+firstToken+generation+tools; column 2 loses setup too.
    expect(chart.columns[0].visibleMs).toBe(200 + 500 + 400 + 4000 + 800);
    expect(chart.columns[1].visibleMs).toBe(500 + 400 + 4000);
    expect(chart.maxVisibleMs).toBe(chart.columns[0].visibleMs);
    expect(chart.columns[0].heightFraction).toBe(1);
    expect(chart.columns[1].heightFraction).toBeCloseTo(
      chart.columns[1].visibleMs / chart.columns[0].visibleMs,
    );
    // Everything hidden: no height driver, but the data still exists.
    const blind = timingChart([row()], new Set(TIMING_SEGMENTS.map((segment) => segment.key)));
    expect(blind.maxVisibleMs).toBe(0);
    expect(blind.columns[0].heightFraction).toBe(0);
    expect(blind.hasData).toBe(true);
  });
});

describe("tokenChart", () => {
  it("splits prompt and completion into four non-overlapping segments", () => {
    const chart = tokenChart([
      row({ usage: { prompt: 1000, cached: 400, completion: 300, reasoning: 100 } }),
    ]);
    const [column] = chart.columns;
    expect(column.uncachedInput).toBe(600);
    expect(column.cached).toBe(400);
    expect(column.visibleOutput).toBe(200);
    expect(column.reasoning).toBe(100);
    expect(column.totalTokens).toBe(1300);
    expect(Object.values(tokenValues(column)).reduce((sum, value) => sum + value, 0)).toBe(1300);
    expect(column.cachePct).toBe(40);
  });

  it("clamps inconsistent provider reports so the total stays prompt + completion", () => {
    const [column] = tokenChart([
      row({ usage: { prompt: 100, cached: 500, completion: 100, reasoning: 500 } }),
    ]).columns;
    expect(column.uncachedInput).toBe(0);
    expect(column.cached).toBe(100);
    expect(column.visibleOutput).toBe(0);
    expect(column.reasoning).toBe(100);
    expect(column.totalTokens).toBe(200);
    expect(Object.values(tokenValues(column)).reduce((sum, value) => sum + value, 0)).toBe(200);
  });

  it("reports cache reuse as sum(cached)/sum(prompt), not a row average", () => {
    const chart = tokenChart([
      row({ id: 1, usage: { prompt: 100, cached: 100, completion: 0, reasoning: 0 } }),
      row({ id: 2, usage: { prompt: 1000, cached: 0, completion: 0, reasoning: 0 } }),
    ]);
    // Row percentages would average to 50%; the token-weighted share is 100/1100.
    expect(chart.summary.cacheReusePct).toBe(9.1);
    expect(chart.summary.promptTotal).toBe(1100);
    expect(chart.summary.cachedTotal).toBe(100);
  });

  it("keeps missing usage distinct from zero usage", () => {
    const chart = tokenChart([row({ usage: null, is_complete: false })]);
    expect(chart.columns[0].hasUsage).toBe(false);
    expect(chart.summary.calls).toBe(0);
    expect(chart.summary.missingUsage).toBe(1);
    expect(chart.summary.cacheReusePct).toBeNull();
    expect(chart.hasData).toBe(false);
  });

  it("rescales visible token columns when a legend series is hidden", () => {
    const chart = tokenChart(
      [
        row({ id: 1, usage: { prompt: 1000, cached: 400, completion: 300, reasoning: 100 } }),
        row({ id: 2, usage: { prompt: 500, cached: 100, completion: 100, reasoning: 0 } }),
      ],
      new Set(["cachedInput"]),
    );
    expect(chart.columns[0].visibleTokens).toBe(600 + 200 + 100);
    expect(chart.columns[1].visibleTokens).toBe(400 + 100);
    expect(chart.columns[0].heightFraction).toBe(1);
    expect(chart.columns[1].heightFraction).toBeCloseTo(500 / 900);
  });

  it("returns no percentage for a zero prompt", () => {
    expect(cacheReusePct(10, 0)).toBeNull();
    expect(cacheReusePct(500, 100)).toBe(100);
  });
});

describe("cost buckets", () => {
  it("chooses minute, hour, or day buckets from the span", () => {
    expect(costBucketUnit(0)).toBe("minute");
    expect(costBucketUnit(2 * 3_600_000)).toBe("minute");
    expect(costBucketUnit(2 * 3_600_000 + 1)).toBe("hour");
    expect(costBucketUnit(3 * 86_400_000)).toBe("hour");
    expect(costBucketUnit(3 * 86_400_000 + 1)).toBe("day");
  });

  it("aligns buckets to UTC units and labels them readably", () => {
    const minuteStart = costBucketStart(BASE + 3 * MINUTE + 30_000, "minute");
    expect(minuteStart).toBe(BASE + 3 * MINUTE);
    expect(costBucketLabel(minuteStart, "minute")).toBe("14:03");
    expect(costBucketStart(BASE + 90_000, "hour")).toBe(BASE);
    expect(costBucketLabel(BASE, "hour")).toBe("14:00");
    expect(costBucketLabel(BASE, "day")).toBe("Nov 2");
    expect(costBucketTitle(BASE, BASE + MINUTE, "minute")).toContain("14:00");
    expect(costBucketTitle(BASE, BASE + MINUTE, "minute")).toContain("14:01");
    expect(costBucketTitle(BASE, BASE + 86_400_000, "day")).toContain("UTC day");
  });

  it("fills gaps so the x axis is a continuous timeline", () => {
    const chart = costChart(
      [row({ id: 1, ts_ms: BASE + 10_000 }), row({ id: 2, ts_ms: BASE + 3 * MINUTE + 30_000 })],
      "agent",
    );
    expect(chart.unit).toBe("minute");
    expect(chart.buckets.map((bucket) => bucket.startMs)).toEqual([
      BASE,
      BASE + MINUTE,
      BASE + 2 * MINUTE,
      BASE + 3 * MINUTE,
    ]);
    expect(chart.buckets.map((bucket) => bucket.calls)).toEqual([1, 0, 0, 1]);
  });
});

describe("costChart", () => {
  it("stacks estimated spend by agent, biggest total first", () => {
    const chart = costChart(
      [
        row({ id: 1, cost_micros: 900, attribution: attribution({ actor_kind: "lead" }) }),
        row({
          id: 2,
          cost_micros: 100,
          attribution: attribution({
            actor_kind: "worker",
            worker_id: "w1",
            agent_name: "traceCharts",
          }),
        }),
        row({ id: 3, cost_micros: 500, attribution: null }),
      ],
      "agent",
    );
    expect(chart.series.map((series) => series.label)).toEqual(["Lead", "Unattributed", "traceCharts"]);
    expect(chart.totalCostMicros).toBe(1500);
    expect(chart.pricedCalls).toBe(3);
    expect(chart.buckets[0].calls).toBe(3);
    expect(chart.buckets[0].costMicros).toBe(1500);
    expect(chart.buckets[0].visibleCostMicros).toBe(1500);
    expect(chart.buckets[0].heightFraction).toBe(1);
  });

  it("stacks by model with the short model label", () => {
    const chart = costChart(
      [
        row({ id: 1, model: "anthropic/claude-opus-5", cost_micros: 300 }),
        row({ id: 2, model: "zai/kimi-k3.1-pro", cost_micros: 700 }),
      ],
      "model",
    );
    expect(chart.series.map((series) => series.label)).toEqual([
      "kimi-k3.1-pro",
      "claude-opus-5",
    ]);
    expect(chart.buckets[0].seriesCosts.map((slice) => slice.costMicros)).toEqual([700, 300]);
  });

  it("counts unpriced and still-running calls separately, never as $0", () => {
    const chart = costChart(
      [
        row({
          id: 1,
          cost_micros: null,
          priced: false,
          usage: { prompt: 100, completion: 50, reasoning: 0, cached: 0 },
        }),
        row({ id: 2, cost_micros: null, priced: false, is_complete: false }),
      ],
      "agent",
    );
    const [bucket] = chart.buckets;
    expect(chart.pricedCalls).toBe(0);
    expect(chart.series).toEqual([]);
    expect(bucket.unpricedCalls).toBe(1);
    expect(bucket.unpricedTokens).toBe(150);
    expect(bucket.liveCalls).toBe(1);
    expect(chart.allUnpriced).toBe(true);
    expect(bucketCostLabel(bucket)).toBe("—");
  });

  it("keeps a genuinely free (priced-at-zero) call distinct from an unpriced one", () => {
    const chart = costChart([row({ cost_micros: 0, priced: true })], "agent");
    expect(chart.pricedCalls).toBe(1);
    expect(chart.unpricedCalls).toBe(0);
    expect(chart.allUnpriced).toBe(false);
    expect(bucketCostLabel(chart.buckets[0])).toBe("≈ $0.00");
  });

  it("drops hidden series from the height driver but not from the total", () => {
    const chart = costChart(
      [
        row({
          id: 1,
          cost_micros: 900,
          attribution: attribution({ actor_kind: "lead" }),
        }),
        row({
          id: 2,
          cost_micros: 100,
          attribution: attribution({
            actor_kind: "worker",
            worker_id: "w1",
            agent_name: "traceCharts",
          }),
        }),
      ],
      "agent",
      new Set(["lead"]),
    );
    expect(chart.totalCostMicros).toBe(1000);
    expect(chart.buckets[0].visibleCostMicros).toBe(100);
    expect(chart.buckets[0].heightFraction).toBe(1);
  });

  it("sums tokens per bucket including cached input and reasoning once", () => {
    const chart = costChart([row({ usage: { prompt: 1000, cached: 400, completion: 200, reasoning: 50 } })], "agent");
    expect(chart.buckets[0].tokens).toBe(1200);
    expect(chart.tokens).toBe(1200);
  });
});

describe("attributionLabel", () => {
  it("names the actor by kind and keeps worker names readable", () => {
    expect(attributionLabel(null)).toBe("Unattributed");
    expect(attributionLabel(undefined)).toBe("Unattributed");
    expect(attributionLabel(attribution())).toBe("Lead");
    expect(attributionLabel(attribution({ actor_kind: "worker", agent_name: "traceCharts" }))).toBe(
      "traceCharts",
    );
    expect(
      attributionLabel(
        attribution({ actor_kind: "worker", agent_name: "", worker_id: "w-17" }),
      ),
    ).toBe("w-17");
    expect(attributionLabel(attribution({ actor_kind: "subagent", agent_name: "researcher" }))).toBe(
      "researcher",
    );
    expect(attributionLabel(attribution({ actor_kind: "system" }))).toBe("System");
    // A kind the frozen union does not know yet must not break the label.
    expect(attributionLabel(attribution({ actor_kind: "future-kind" } as unknown as Partial<TraceAttribution>))).toBe("Unattributed");
  });
});
