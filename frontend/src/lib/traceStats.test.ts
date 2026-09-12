// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";
import {
  PHASE_SEGMENTS,
  TOKEN_SEGMENTS,
  cacheHitPct,
  chartVisibility,
  columnHeightPct,
  fmtInt,
  fmtPhase,
  latestRows,
  phaseHeightVisibility,
  phaseMsByKey,
  phaseSeries,
  phaseTipRows,
  stallSharePct,
  stackSegments,
  stackTotal,
  tokenHeightVisibility,
  tokenMsByKey,
  tokenSeries,
  tokenTipRows,
  traceSummary,
} from "./traceStats";
import type { LlmRequestSummary } from "./types";

/** A full-shape summary row with sensible defaults. */
function row(id: number, overrides: Partial<LlmRequestSummary> = {}): LlmRequestSummary {
  return {
    id,
    ts_ms: 1_700_000_000_000 + id * 1000,
    model: "m",
    base_url: "http://u/v1/",
    provider: "p",
    http_status: 200,
    usage: null,
    finish_reason: "stop",
    is_complete: true,
    version: 0,
    ttft_ms: null,
    generation_ms: null,
    reasoning_ms: null,
    connect_ms: null,
    tools_ms: null,
    prep_ms: null,
    compact_ms: null,
    backoff_ms: null,
    stall_ms: null,
    error: null,
    cancelled: false,
    response_truncated: false,
    response_evicted: false,
    guard_cut: null,
    ...overrides,
  };
}

/** A list of n consecutive rows (ids 1..n, oldest-first). */
function rows(n: number): LlmRequestSummary[] {
  return Array.from({ length: n }, (_, i) => row(i + 1));
}

describe("latestRows", () => {
  it("keeps oldest-first order and returns everything under the cap", () => {
    expect(latestRows(rows(3), 10).map((r) => r.id)).toEqual([1, 2, 3]);
  });

  it("slices to the most recent N when over the cap", () => {
    expect(latestRows(rows(10), 3).map((r) => r.id)).toEqual([8, 9, 10]);
  });

  it("handles the empty list", () => {
    expect(latestRows([], 5)).toEqual([]);
  });
});

describe("phaseSeries", () => {
  it("maps null phases to 0 and sums the total", () => {
    const rs = [
      row(1, { connect_ms: 1100, ttft_ms: null, generation_ms: 5500, tools_ms: 12 }),
      row(2),
    ];
    const s = phaseSeries(rs);
    expect(s[0]).toMatchObject({
      id: 1,
      connectMs: 1100,
      waitMs: 0,
      genMs: 5500,
      toolsMs: 12,
      totalMs: 6612,
    });
    expect(s[1].totalMs).toBe(0);
  });

  it("subtracts the inclusive compact window from the prep segment", () => {
    // prep_ms includes compact_ms by design (the compaction call runs inside
    // the prep window) — the stacked bar must not double-count it (review
    // finding 2026-08-22: disjoint segments, honest totalMs).
    const rs = [
      row(1, { prep_ms: 2000, compact_ms: 500, connect_ms: 100 }),
      // compact > prep is never expected — clamped to 0, never negative.
      row(2, { prep_ms: 100, compact_ms: 500 }),
    ];
    const s = phaseSeries(rs);
    expect(s[0]).toMatchObject({ prepMs: 1500, compactMs: 500, totalMs: 2100 });
    expect(s[1]).toMatchObject({ prepMs: 0, compactMs: 500, totalMs: 500 });
  });

  it("splits reasoning out of the generation segment", () => {
    // reasoning_ms is a subset of generation_ms — the chart shows a purple
    // reason segment and generate as the remainder (2026-08-22 split).
    const s = phaseSeries([row(1, { reasoning_ms: 2000, generation_ms: 5000 })]);
    expect(s[0]).toMatchObject({
      genMs: 5000,
      reasoningMs: 2000,
      totalMs: 5000, // reasoning is INSIDE generation — total unchanged
    });
    expect(phaseMsByKey(s[0]).generate).toBe(3000);
  });

  it("clamps the generate segment to 0 when reasoning exceeds generation", () => {
    // A provider quirk (reasoning reported wider than generation) must never
    // render a negative generate segment.
    const s = phaseSeries([row(1, { reasoning_ms: 7000, generation_ms: 5000 })]);
    expect(s[0]).toMatchObject({ reasoningMs: 7000, totalMs: 5000 });
    expect(phaseMsByKey(s[0]).generate).toBe(0);
  });

  it("absent reasoning_ms leaves the whole generation as generate", () => {
    // A non-reasoning request has no reason segment — generate == generation.
    const s = phaseSeries([row(1, { generation_ms: 5000 })]);
    expect(s[0]).toMatchObject({ reasoningMs: 0, genMs: 5000 });
    expect(phaseMsByKey(s[0]).generate).toBe(5000);
  });

  it("stall counts as generate (the generate segment includes the stalled byte-silence)", () => {
    // stall_ms is a subset of generation_ms (byte-silence gaps inside the
    // streaming window) — it is NOT subtracted from generate; it renders as
    // the dithered hatch inside the generate segment instead.
    const s = phaseSeries([row(1, { generation_ms: 5000, stall_ms: 400 })]);
    expect(s[0]).toMatchObject({ genMs: 5000, stallMs: 400 });
    expect(phaseMsByKey(s[0]).generate).toBe(5000);
    expect(phaseMsByKey(s[0]).stall).toBe(400);
  });

  it("the stall chip is height-neutral (phaseHeightVisibility excludes stall)", () => {
    // Toggling the stall overlay must not change the column height — the
    // height driver sums prep..tools WITHOUT stall (mirrors the cached chip).
    const r = phaseSeries([row(1, { generation_ms: 5000, stall_ms: 400 })])[0];
    const shown = chartVisibility(PHASE_SEGMENTS, new Set());
    const stallHidden = chartVisibility(PHASE_SEGMENTS, new Set(["stall"]));
    const withStall = stackTotal(phaseMsByKey(r), phaseHeightVisibility(shown));
    const withoutStall = stackTotal(phaseMsByKey(r), phaseHeightVisibility(stallHidden));
    expect(withStall).toBe(withoutStall);
    // …and the height total excludes stall even when the chip is shown.
    expect(withStall).toBe(stackTotal(phaseMsByKey(r), { ...shown, stall: false }));
  });

  it("stallSharePct: coverage of the generate segment, clamped to 100", () => {
    // 400ms stalled of a 5000ms generate window → 8%.
    expect(stallSharePct(phaseSeries([row(1, { generation_ms: 5000, stall_ms: 400 })])[0])).toBe(8);
    // A stall during the reasoning window is counted in BOTH reasoning_ms
    // and stall_ms, so stall can exceed the generate remainder — clamp to 100.
    expect(
      stallSharePct(
        phaseSeries([row(2, { generation_ms: 5000, reasoning_ms: 4900, stall_ms: 400 })])[0],
      ),
    ).toBe(100);
    // No stall → no hatch.
    expect(stallSharePct(phaseSeries([row(3, { generation_ms: 5000 })])[0])).toBe(0);
  });

  it("covers the order the backend returns (oldest-first stays oldest-first)", () => {
    const s = phaseSeries(rows(4));
    expect(s.map((r) => r.id)).toEqual([1, 2, 3, 4]);
  });
});

describe("tokenSeries", () => {
  it("maps null usage to zeros", () => {
    const rs = [
      row(1, { usage: { prompt: 1000, cached: 800, completion: 50, reasoning: 20 } }),
      row(2),
    ];
    const s = tokenSeries(rs);
    expect(s[0]).toMatchObject({ prompt: 1000, cached: 800, completion: 50, reasoning: 20 });
    expect(s[1]).toMatchObject({ prompt: 0, cached: 0, completion: 0, reasoning: 0 });
  });
});

describe("cacheHitPct", () => {
  it("returns null when unmeasurable (no usage or zero prompt)", () => {
    expect(cacheHitPct(null)).toBeNull();
    expect(cacheHitPct({ prompt: 0, cached: 0, completion: 0, reasoning: 0 })).toBeNull();
  });

  it("rounds the cached/prompt ratio to 1 decimal (fmtPct display)", () => {
    expect(cacheHitPct({ prompt: 1000, cached: 993, completion: 0, reasoning: 0 })).toBe(99.3);
  });

  it("clamps to 100 when a provider reports cached > prompt", () => {
    expect(cacheHitPct({ prompt: 100, cached: 125, completion: 0, reasoning: 0 })).toBe(100);
  });
});

describe("traceSummary", () => {
  it("sums totals, computes tok/s and averages cache hits over measurable rows", () => {
    const rs = [
      row(1, {
        connect_ms: 1000,
        ttft_ms: 5,
        generation_ms: 10000,
        tools_ms: 500,
        usage: { prompt: 1000, cached: 900, completion: 100, reasoning: 0 },
      }),
      row(2, {
        connect_ms: 2000,
        generation_ms: 10000,
        tools_ms: 300,
        usage: { prompt: 500, cached: 250, completion: 50, reasoning: 10 },
      }),
      row(3, { generation_ms: 5000 }),
    ];
    const s = traceSummary(rs);
    expect(s.count).toBe(3);
    expect(s.totalConnectMs).toBe(3000);
    expect(s.totalWaitMs).toBe(5);
    expect(s.totalGenMs).toBe(25000);
    expect(s.totalToolsMs).toBe(800);
    expect(s.totalPrompt).toBe(1500);
    expect(s.totalCached).toBe(1150);
    expect(s.totalCompletion).toBe(150);
    // 150 completion tokens over 25 s = 6.0 tok/s.
    expect(s.avgTokPerSec).toBe(6);
    // (90 + 50) / 2 = 70 — row 3 is unmeasurable and excluded.
    expect(s.avgCacheHitPct).toBe(70);
  });

  it("returns null averages when nothing is measurable", () => {
    const s = traceSummary([row(1), row(2)]);
    expect(s.avgTokPerSec).toBeNull();
    expect(s.avgCacheHitPct).toBeNull();
  });
});

describe("columnHeightPct", () => {
  it("fill mode normalizes every non-zero column to 100", () => {
    expect(columnHeightPct(30_000, 120_000, "fill")).toBe(100);
    expect(columnHeightPct(120_000, 120_000, "fill")).toBe(100);
  });

  it("fill mode maps a zero total to 0", () => {
    expect(columnHeightPct(0, 120_000, "fill")).toBe(0);
  });

  it("relative mode scales by the series max — 2 min is 4× taller than 30 s", () => {
    // The acceptance example for the Relative toggle: a 2-minute request's
    // column is 4× the height of a 30-second one (100% vs 25%).
    expect(columnHeightPct(120_000, 120_000, "relative")).toBe(100);
    expect(columnHeightPct(30_000, 120_000, "relative")).toBe(25);
  });

  it("relative mode maps a zero total to 0", () => {
    expect(columnHeightPct(0, 120_000, "relative")).toBe(0);
  });

  it("relative mode guards against a zero max (empty/all-zero series)", () => {
    expect(columnHeightPct(0, 0, "relative")).toBe(0);
    expect(columnHeightPct(5, 0, "relative")).toBe(0);
  });
});

describe("chartVisibility / tokenHeightVisibility", () => {
  it("maps every palette key per the hidden set (unknown keys ignored)", () => {
    const v = chartVisibility(PHASE_SEGMENTS, new Set(["wait", "tools", "bogus"]));
    expect(v).toEqual({
      prep: true,
      compact: true,
      backoff: true,
      connect: true,
      wait: false,
      reason: true,
      generate: true,
      stall: true,
      tools: false,
    });
  });

  it("token height sums prompt + completion + reasoning — cached is NOT additive", () => {
    // Ports the old tokenStackTotal tests: cached (800) is a subset of
    // prompt, overlaid inside the prompt bar, so the height driver must not
    // double-count it (review High 1 of the 2026-12 legend toggles).
    const r = { id: 1, label: "x", prompt: 1000, cached: 800, completion: 50, reasoning: 20 };
    const v = tokenHeightVisibility(chartVisibility(TOKEN_SEGMENTS, new Set()));
    expect(stackTotal(tokenMsByKey(r), v)).toBe(1070);
    expect(
      stackTotal(
        tokenMsByKey({ id: 1, label: "x", prompt: 0, cached: 0, completion: 0, reasoning: 0 }),
        v,
      ),
    ).toBe(0);
  });

  it("toggling the cached chip is height-neutral; hiding prompt drops only prompt", () => {
    const r = { id: 1, label: "x", prompt: 1000, cached: 800, completion: 50, reasoning: 20 };
    const cachedHidden = chartVisibility(TOKEN_SEGMENTS, new Set(["cached"]));
    expect(stackTotal(tokenMsByKey(r), tokenHeightVisibility(cachedHidden))).toBe(1070);
    const promptHidden = chartVisibility(TOKEN_SEGMENTS, new Set(["prompt"]));
    // cached (a subset of the hidden prompt) contributes nothing either —
    // completion + reasoning only.
    expect(stackTotal(tokenMsByKey(r), tokenHeightVisibility(promptHidden))).toBe(70);
  });

  it("an all-hidden token stack totals 0 (drives the empty-state hint)", () => {
    const r = { id: 1, label: "x", prompt: 1000, cached: 800, completion: 50, reasoning: 20 };
    const all = chartVisibility(
      TOKEN_SEGMENTS,
      new Set(["prompt", "cached", "completion", "reasoning"]),
    );
    expect(stackTotal(tokenMsByKey(r), tokenHeightVisibility(all))).toBe(0);
  });
});

describe("phaseMsByKey / tokenMsByKey", () => {
  it("extracts every phase field keyed by PHASE_SEGMENTS key", () => {
    const [r] = phaseSeries([
      row(1, {
        prep_ms: 120,
        compact_ms: 20,
        backoff_ms: 1000,
        connect_ms: 45,
        ttft_ms: 600,
        generation_ms: 8400,
        reasoning_ms: 2000,
        stall_ms: 400,
        tools_ms: 12,
      }),
    ]);
    expect(phaseMsByKey(r)).toEqual({
      prep: 100, // prep_ms − compact_ms
      compact: 20,
      backoff: 1000,
      connect: 45,
      wait: 600,
      reason: 2000,
      generate: 6400, // genMs − reasoningMs (stall included, hatched overlay)
      stall: 400,
      tools: 12,
    });
    // The extractor keys line up with the palette the legend/chart use.
    expect(Object.keys(phaseMsByKey(r))).toEqual(PHASE_SEGMENTS.map((s) => s.key));
  });

  it("extracts every token field keyed by TOKEN_SEGMENTS key", () => {
    const [r] = tokenSeries([
      row(1, { usage: { prompt: 1000, cached: 800, completion: 50, reasoning: 20 } }),
    ]);
    expect(tokenMsByKey(r)).toEqual({ prompt: 1000, cached: 800, completion: 50, reasoning: 20 });
    expect(Object.keys(tokenMsByKey(r))).toEqual(TOKEN_SEGMENTS.map((s) => s.key));
  });
});

describe("stackSegments", () => {
  const segs = [
    { key: "a", label: "A", cls: "bg-a" },
    { key: "b", label: "B", cls: "bg-b" },
    { key: "c", label: "C", cls: "bg-c" },
  ];

  it("keeps palette order and drops zero-value segments", () => {
    expect(stackSegments(segs, { a: 10, b: 0, c: 5 }, {})).toEqual([
      { key: "a", label: "A", cls: "bg-a", value: 10 },
      { key: "c", label: "C", cls: "bg-c", value: 5 },
    ]);
  });

  it("excludes segments hidden in the visibility map", () => {
    expect(stackSegments(segs, { a: 10, b: 20, c: 5 }, { a: false })).toEqual([
      { key: "b", label: "B", cls: "bg-b", value: 20 },
      { key: "c", label: "C", cls: "bg-c", value: 5 },
    ]);
  });

  it("treats absent visibility keys as visible (future segments default on)", () => {
    expect(stackSegments(segs, { a: 10, b: 20, c: 5 }, {})).toHaveLength(3);
  });

  it("returns an empty list when every segment is hidden", () => {
    expect(stackSegments(segs, { a: 10, b: 20, c: 5 }, { a: false, b: false, c: false })).toEqual(
      [],
    );
  });
});

describe("stackTotal", () => {
  it("sums only the visible values", () => {
    expect(stackTotal({ a: 10, b: 20, c: 5 }, { a: false })).toBe(25);
    expect(stackTotal({ a: 10, b: 20, c: 5 }, {})).toBe(35);
  });

  it("is 0 when every segment is hidden", () => {
    expect(stackTotal({ a: 10, b: 20 }, { a: false, b: false })).toBe(0);
  });

  it("hiding a tall segment changes the total (the Relative rescale driver)", () => {
    // 900ms of wait hidden → the remaining 100ms becomes the whole column.
    expect(stackTotal({ wait: 900, prep: 100 }, { wait: false })).toBe(100);
  });
});

describe("fmtPhase / fmtInt", () => {
  it("formats durations as ms / s / m:s", () => {
    expect(fmtPhase(120)).toBe("120ms");
    expect(fmtPhase(2000)).toBe("2.0s");
    expect(fmtPhase(65_000)).toBe("1m 5s");
  });

  it("comma-groups integers", () => {
    expect(fmtInt(12_345)).toBe("12,345");
  });
});

describe("phaseTipRows", () => {
  it("puts total first, keeps stack order, and colors rows to match the segments", () => {
    const [r] = phaseSeries([
      row(1, {
        prep_ms: 120,
        compact_ms: 20,
        backoff_ms: 1000,
        connect_ms: 45,
        ttft_ms: 600,
        generation_ms: 8400,
        reasoning_ms: 2000,
        stall_ms: 400,
        tools_ms: 12,
      }),
    ]);
    const rows = phaseTipRows(r);
    expect(rows.map((x) => x.label)).toEqual([
      "total", "prep", "compact", "backoff", "connect", "wait", "reason", "generate", "stall", "tools",
    ]);
    // total = 100 (prep 120 − compact 20) + 20 + 1000 + 45 + 600 + 8400 + 12 = 10177ms
    expect(rows[0].value).toBe("10.2s");
    expect(rows.find((x) => x.label === "prep")?.value).toBe("100ms");
    expect(rows.find((x) => x.label === "reason")?.value).toBe("2.0s");
    expect(rows.find((x) => x.label === "generate")?.value).toBe("6.4s");
    expect(rows.find((x) => x.label === "wait")?.cls).toBe("bg-slate-500/80");
  });

  it("renders zero phases as an em dash so the tooltip layout stays stable", () => {
    const [r] = phaseSeries([row(1, { prep_ms: 50 })]);
    const rows = phaseTipRows(r);
    expect(rows.find((x) => x.label === "compact")?.value).toBe("—");
    expect(rows.find((x) => x.label === "tools")?.value).toBe("—");
    expect(rows.find((x) => x.label === "prep")?.value).toBe("50ms");
  });
});

describe("tokenTipRows", () => {
  it("formats the token stack with the cached share as count + percent", () => {
    const [r] = tokenSeries([
      row(1, { usage: { prompt: 12_345, completion: 456, reasoning: 789, cached: 6_789 } }),
    ]);
    const rows = tokenTipRows(r);
    expect(rows.map((x) => x.label)).toEqual(["prompt", "cached", "completion", "reasoning"]);
    expect(rows[0]).toMatchObject({ value: "12,345", cls: "bg-sky-500/70" });
    // 6789/12345 = 54.99% → rounds to "55" (integer stays integral).
    expect(rows[1]).toMatchObject({ value: "6,789 (55%)", cls: "bg-emerald-500/70" });
    expect(rows[2].value).toBe("456");
    expect(rows[3].value).toBe("789");
  });

  it("shows a fractional cached share with one decimal", () => {
    const [r] = tokenSeries([
      row(1, { usage: { prompt: 1000, completion: 0, reasoning: 0, cached: 993 } }),
    ]);
    const rows = tokenTipRows(r);
    expect(rows[1]).toMatchObject({ value: "993 (99.3%)", cls: "bg-emerald-500/70" });
  });

  it("renders zero/unknown counts as em dashes", () => {
    const [r] = tokenSeries([row(1)]); // usage: null
    const rows = tokenTipRows(r);
    expect(rows[0].value).toBe("0");
    expect(rows[1].value).toBe("—");
    expect(rows[2].value).toBe("—");
  });
});
