import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";
import TraceCharts from "./TraceCharts.svelte";
import type { TraceAttribution, TraceRow } from "../lib/traceCharts";

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
    model: "zai/kimi-k3.1-pro",
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

describe("TraceCharts", () => {
  it("states the chart scope and opens on the Timing tab", () => {
    const { container } = render(TraceCharts, {
      props: { rows: [row(), row({ id: 2, ts_ms: BASE + MINUTE })], scopeLabel: "this session" },
    });

    expect(container.querySelector(".scope")?.textContent?.trim()).toBe(
      "Charts for 2 filtered calls · this session",
    );
    expect(screen.getByRole("tab", { name: "Timing" }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByText("Where did request time go?")).toBeTruthy();
  });

  it("switches to the Tokens tab with a token-weighted cache-reuse summary", async () => {
    const { container } = render(TraceCharts, {
      props: {
        rows: [
          row({ id: 1, usage: { prompt: 100, cached: 100, completion: 0, reasoning: 0 } }),
          row({ id: 2, ts_ms: BASE + MINUTE, usage: { prompt: 1000, cached: 0, completion: 0, reasoning: 0 } }),
        ],
        scopeLabel: "this session",
      },
    });

    await fireEvent.click(screen.getByRole("tab", { name: "Tokens" }));

    expect(screen.getByText("What did the tokens go to?")).toBeTruthy();
    // sum(cached)/sum(prompt) = 100/1100, not the 50% row average.
    expect(container.textContent).toContain("Cache reuse 9.1%");
  });

  it("hides a legend series when its chip is toggled", async () => {
    render(TraceCharts, { props: { rows: [row()], scopeLabel: "this session" } });

    const chip = screen.getByRole("button", { name: "Setup" });
    expect(chip.getAttribute("aria-pressed")).toBe("true");
    await fireEvent.click(chip);
    expect(screen.getByRole("button", { name: "Setup" }).getAttribute("aria-pressed")).toBe("false");
  });

  it("offers the chart values in an accessible table, not only on hover", () => {
    const { container } = render(TraceCharts, {
      props: { rows: [row()], scopeLabel: "this session" },
    });

    const details = container.querySelector("details.values");
    expect(details?.textContent).toContain("Values table");
    const table = details?.querySelector("table");
    expect(table?.textContent).toContain("Compaction (in Setup)");
    expect(table?.textContent).toContain("First token");
    expect(table?.textContent).toContain("400 ms");
    expect(table?.querySelectorAll("tbody tr")).toHaveLength(1);
  });

  it("emits a bucket's half-open time range when a cost column is selected", async () => {
    const onbucket = vi.fn();
    render(TraceCharts, {
      props: {
        rows: [row({ id: 1 }), row({ id: 2, ts_ms: BASE + 30_000 })],
        scopeLabel: "this session",
        onbucket,
      },
    });

    await fireEvent.click(screen.getByRole("tab", { name: "Cost" }));
    const bucket = screen.getByRole("button", { name: /^Filter to/ });
    await fireEvent.click(bucket);

    expect(onbucket).toHaveBeenCalledTimes(1);
    expect(onbucket).toHaveBeenCalledWith(BASE, BASE + MINUTE);
  });

  it("shows unpriced calls as a note and a dash, never as $0", async () => {
    const { container } = render(TraceCharts, {
      props: {
        rows: [row({ cost_micros: null, priced: false })],
        scopeLabel: "this session",
      },
    });

    await fireEvent.click(screen.getByRole("tab", { name: "Cost" }));

    expect(container.textContent).toContain("No priced calls in this scope.");
    expect(container.textContent).toContain("no price and");
    expect(screen.getByRole("button", { name: "Unpriced" })).toBeTruthy();
    const table = container.querySelector("table");
    expect(table?.textContent).toContain("—");
    expect(table?.textContent).not.toContain("$0.00");
  });

  it("switches the cost stack between agents and models", async () => {
    render(TraceCharts, {
      props: {
        rows: [
          row({ id: 1, model: "zai/kimi-k3.1-pro", cost_micros: 700 }),
          row({ id: 2, model: "anthropic/claude-opus-5", cost_micros: 300 }),
        ],
        scopeLabel: "this session",
      },
    });

    await fireEvent.click(screen.getByRole("tab", { name: "Cost" }));
    expect(screen.getByRole("button", { name: "Lead" })).toBeTruthy();

    const model = screen.getByRole("button", { name: "Model" });
    await fireEvent.click(model);
    expect(screen.getByRole("button", { name: "Model" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: "kimi-k3.1-pro" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "claude-opus-5" })).toBeTruthy();
  });

  it("collapses and reopens the panel", async () => {
    render(TraceCharts, { props: { rows: [row()], scopeLabel: "this session" } });

    const hide = screen.getByRole("button", { name: "Hide" });
    expect(hide.getAttribute("aria-expanded")).toBe("true");
    await fireEvent.click(hide);
    expect(screen.queryByRole("tablist")).toBeNull();
    expect(screen.getByRole("button", { name: "Show" }).getAttribute("aria-expanded")).toBe("false");
  });

  it("explains an empty scope instead of drawing a blank chart", () => {
    render(TraceCharts, { props: { rows: [], scopeLabel: "this session" } });
    expect(screen.getByText("No model calls in this scope yet.")).toBeTruthy();
  });
});
