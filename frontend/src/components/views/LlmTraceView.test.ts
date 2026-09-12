// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect } from "vitest";
import { shouldStopPolling, clampStatsHeight, rowBadges } from "./LlmTraceView";
import type { LlmRequestDetail } from "../../lib/types";

/**
 * Regression tests for the detail/compare poll-stop predicate (reviews
 * N1 2026-06-14 + L1/F1 2026-08-18): the poll must stop once the record is
 * terminal OR absent — a null (never recorded / ring-evicted) can never
 * become non-null because ids are never reused and the ring only loses
 * records, so re-fetching either only re-clones up to ~2 MiB per tick
 * forever.
 */

function detail(overrides: Partial<LlmRequestDetail> = {}): LlmRequestDetail {
  return {
    id: 7,
    ts_ms: 1,
    model: "m",
    base_url: "http://u/v1/",
    provider: "p",
    request_json: {},
    response_raw: null,
    http_status: null,
    usage: null,
    finish_reason: null,
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
    request_truncated: false,
    response_evicted: false,
    is_complete: false,
    version: 0,
    raw_tool_calls: null,
    guard_cut: null,
    ...overrides,
  };
}

describe("shouldStopPolling (N1 + L1 regression)", () => {
  it("keeps polling an in-flight, incomplete record", () => {
    expect(shouldStopPolling(detail())).toBe(false);
  });

  it("stops on a terminal record (finish_reason present)", () => {
    expect(shouldStopPolling(detail({ is_complete: true, finish_reason: "stop" }))).toBe(true);
  });

  it("stops on a terminal record (error present, no finish_reason)", () => {
    expect(shouldStopPolling(detail({ is_complete: true, error: "HTTP 502" }))).toBe(true);
  });

  it("stops on null — never recorded or ring-evicted (L1)", () => {
    // The compare effect hits this routinely (id-1 of the first request);
    // the detail effect hits it when the selection is evicted out from
    // under the poll. Neither can ever become non-null again.
    expect(shouldStopPolling(null)).toBe(true);
  });

  it("does NOT stop on a non-terminal record whose payload was evicted", () => {
    // Payload eviction (memory budget) is not terminal: the row survives
    // and may still gain a finish/error later.
    expect(shouldStopPolling(detail({ response_evicted: true }))).toBe(false);
  });

  it("does NOT stop on a truncated-but-streaming record", () => {
    expect(shouldStopPolling(detail({ response_truncated: true }))).toBe(false);
  });
});

describe("clampStatsHeight (stats splitter clamp table)", () => {
  it("clamps below the minimum to 140px", () => {
    expect(clampStatsHeight(50, 1000)).toBe(140);
    expect(clampStatsHeight(0, 1000)).toBe(140);
  });

  it("passes heights inside the range through unchanged", () => {
    expect(clampStatsHeight(140, 1000)).toBe(140);
    expect(clampStatsHeight(260, 1000)).toBe(260);
    expect(clampStatsHeight(700, 1000)).toBe(700);
  });

  it("clamps above 70% of the tab height", () => {
    expect(clampStatsHeight(900, 1000)).toBe(700);
    expect(clampStatsHeight(100000, 1000)).toBe(700);
  });

  it("rounds fractional drag positions", () => {
    expect(clampStatsHeight(250.6, 1000)).toBe(251);
  });
});

describe("rowBadges (D1 cancelled/failed badge table)", () => {
  it("flags failed for an error record", () => {
    expect(rowBadges({ error: "boom", http_status: 200, cancelled: false })).toEqual({
      failed: true,
      cancelled: false,
    });
  });

  it("flags failed for an HTTP failure status", () => {
    expect(rowBadges({ error: null, http_status: 502, cancelled: false })).toEqual({
      failed: true,
      cancelled: false,
    });
  });

  it("flags cancelled for a consumer-drop record", () => {
    expect(rowBadges({ error: null, http_status: 200, cancelled: true })).toEqual({
      failed: false,
      cancelled: true,
    });
  });

  it("keeps ERR (not CANCELLED) when a genuine error landed before the drop", () => {
    expect(rowBadges({ error: "boom", http_status: 200, cancelled: true })).toEqual({
      failed: true,
      cancelled: false,
    });
  });

  it("flags neither for a healthy completed record", () => {
    expect(rowBadges({ error: null, http_status: 200, cancelled: false })).toEqual({
      failed: false,
      cancelled: false,
    });
  });
});
