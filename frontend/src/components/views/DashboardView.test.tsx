// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Dashboard view rendering (backlog 652ae094). Markup pinned via
 * renderToStaticMarkup (the BacklogItemCard/PlanStepRow pattern — vitest runs in
 * a node environment, no React DOM test infra). The Dashboard's fetch wrapper
 * resolves inside useEffect, which never runs under static rendering, so these
 * tests drive the exported presentational body (`DashboardBody`) with fixtures.
 *
 * The load-bearing distinction under test: the METERED token figures come from
 * the savings ledger and carry no dollar value, while the dollar figures are
 * ESTIMATED from the [[pricing]] table and stay in the cache section, apart
 * from the metered totals.
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { DashboardBody } from "./DashboardView";
import { fmtTokens } from "../../lib/format";
import type { PricingEntry, ProjectStats, SavingsStats } from "../../lib/tauri";
import viewSource from "./DashboardView.tsx?raw";

/** A pricing table where 1M uncached input tokens cost $3, 1M cached $0.30. */
const pricing: PricingEntry[] = [
  { model: "test-model", input_per_1m: 3, output_per_1m: 15, cached_per_1m: 0.3 },
];

/** 1M prompt tokens, 400K of them cached — so the project's realized uncached
 *  input rate is exactly $3/1M and its cached rate exactly $0.30/1M. */
const project: ProjectStats = {
  prompt_tokens: 1_000_000,
  completion_tokens: 0,
  reasoning_tokens: 0,
  cached_tokens: 400_000,
  ttft_ms_total: 0,
  generation_ms_total: 0,
  timed_requests: 0,
  request_count: 10,
  session_count: 1,
  per_model: [
    {
      model: "test-model",
      endpoint: "primary",
      prompt_tokens: 1_000_000,
      completion_tokens: 0,
      reasoning_tokens: 0,
      cached_tokens: 400_000,
      ttft_ms_total: 0,
      generation_ms_total: 0,
      timed_requests: 0,
      request_count: 10,
    },
  ],
  per_day: [],
};

const savings: SavingsStats = {
  saved_tokens_total: 123_456,
  event_count: 3,
  per_kind: [
    { kind: "skeleton", event_count: 2, saved_tokens: 111_456 },
    { kind: "archive_expand", event_count: 1, saved_tokens: -4_000 },
  ],
  per_day: [{ day: 19_675, event_count: 3, saved_tokens: 12_345 }],
  recent: [
    {
      id: "e1",
      session_id: "s1",
      kind: "skeleton",
      detail: "src/agent/turn.rs",
      tokens_before: 12_000,
      tokens_after: 900,
      tokens_saved: 11_100,
      measured: false,
      created_at: 1_700_000_000,
    },
  ],
  cache: {
    prompt_tokens: 1_000_000,
    cached_tokens: 400_000,
    cached_not_null_requests: 10,
    request_count: 10,
  },
};

function render(
  s: SavingsStats = savings,
  p: ProjectStats = project,
  pr: PricingEntry[] = pricing,
): string {
  return renderToStaticMarkup(<DashboardBody savings={s} project={p} pricing={pr} />);
}

/**
 * The markup of one `data-testid` card, bounded by the next card's marker —
 * renderToStaticMarkup offers no DOM to walk, so slicing is how a test scopes an
 * assertion to a single section.
 */
function cardMarkup(html: string, testid: string): string {
  const marker = `data-testid="${testid}"`;
  const at = html.indexOf(marker);
  expect(at).toBeGreaterThanOrEqual(0);
  const start = html.lastIndexOf("<div", at);
  const next = html.indexOf('data-testid="', at + marker.length);
  return html.slice(start, next === -1 ? undefined : next);
}

describe("DashboardBody (backlog 652ae094)", () => {
  it("shows the metered totals from the ledger, with no dollar value on them", () => {
    const html = render();
    expect(html).toContain("tokens saved");
    expect(html).toContain("metered from the savings ledger");
    expect(html).toContain(fmtTokens(123_456));
    // The ledger totals are token-only: no dollar figure is attached to them.
    expect(html).not.toContain("$123");
  });

  it("breaks the ledger down by kind, biggest saver first, keeping the negative row", () => {
    // Scoped to the per-kind card: "skeleton" also appears in the recent-events
    // fixture row, so an unscoped assertion would survive the whole table being
    // dropped. The figures and the order are the load-bearing part.
    const card = cardMarkup(render(), "dashboard-per-kind");
    expect(card).toContain("skeleton");
    expect(card).toContain("archive_expand");
    expect(card).toContain(fmtTokens(111_456));
    // The signed rendering that makes a re-expansion read as a subtraction.
    expect(card).toContain(fmtTokens(-4_000));
    // Biggest saver first — the backend's SUM(...) DESC promise.
    expect(card.indexOf("skeleton")).toBeLessThan(card.indexOf("archive_expand"));
  });

  it("lists the recent events with their before → after sizes", () => {
    const html = render();
    expect(html).toContain("src/agent/turn.rs");
    expect(html).toContain(fmtTokens(12_000));
    expect(html).toContain(fmtTokens(900));
  });

  it("prices the ESTIMATED dollars at the project's realized rates", () => {
    const html = render();
    // 123,456 saved tokens at the realized $3/1M uncached input rate → $0.37.
    expect(html).toContain("$0.37");
    // 400K cached tokens served at $0.30/1M instead of $3/1M → $1.08 saved.
    expect(html).toContain("$1.08");
    expect(html).toContain("40.0%"); // cache hit rate
    expect(html).toContain("ESTIMATED");
    expect(html).toContain("never part of the metered totals");
  });

  it("renders an honest empty state on a fresh project", () => {
    const html = render({
      ...savings,
      event_count: 0,
      saved_tokens_total: 0,
      per_kind: [],
      per_day: [],
      recent: [],
    });
    expect(html).toContain("No savings recorded yet.");
    expect(html).toContain("[general.optimizer]");
  });

  it("fabricates no dollar figure when the pricing table is empty", () => {
    const html = render(savings, project, []);
    expect(html).toContain("No [[pricing]] entries in endpoints.toml");
    expect(html).not.toContain("$0.37");
  });

  it("renders the per-day bars off the epoch-DAY bucket", () => {
    // The one cross-layer unit in this view: the backend buckets by epoch DAY
    // (created_at / 86400), so the label multiplies by 86_400_000 ms. Copying
    // StatsView's day-start-seconds × 1000 convention here would shift every
    // label by ~86× and fail exactly this assertion.
    const card = cardMarkup(render(), "dashboard-per-day");
    expect(card).toContain(new Date(19_675 * 86_400_000).toLocaleDateString());
    // 12_345, not the recent row's 12_000 — so the figure can only have come
    // from this card even if the slice bound ever regresses.
    expect(card).toContain(fmtTokens(12_345));
  });

  it("reports unavailable dollars when no pricing entry matches the project's models", () => {
    // A pricing table that exists but covers none of the models this project
    // used: both ESTIMATED figures are "—", so the note must not claim to have
    // priced anything.
    const otherModels: PricingEntry[] = [
      { model: "some-other-model", input_per_1m: 1, output_per_1m: 2, cached_per_1m: 0.1 },
    ];
    const html = render(savings, project, otherModels);
    expect(html).toContain("No [[pricing]] entries for the models this project used");
    expect(html).not.toContain("never part of the metered totals");
  });
});

/**
 * Live update while the tab is open (user report 2027-01-25): the wrapper used
 * to fetch once on mount, so the view stayed stale until it was closed and
 * reopened. Its effect never runs under `renderToStaticMarkup`, so — like
 * InflightBar.test.ts pins its elapsed-timer interval — the wiring is pinned at
 * the source level instead.
 */
describe("DashboardView live refresh (user report 2027-01-25)", () => {
  it("polls the savings ledger while its tab is open", () => {
    // The house 2 s fallback cadence, matching PlanProgress's plan-file poll.
    expect(viewSource).toContain("const POLL_MS = 2000;");
    expect(viewSource).toContain("const interval = setInterval(() => void tick(), POLL_MS);");
    // An open-but-idle view must not poll while the window is hidden...
    expect(viewSource).toContain("document.hidden");
    // ...nor stack IPC calls when a fetch outlives the poll interval.
    expect(viewSource).toContain("inFlight.current");
    // Regaining focus refreshes at once instead of waiting out the interval.
    expect(viewSource).toContain('window.addEventListener("focus", onFocus)');
    expect(viewSource).toContain('document.addEventListener("visibilitychange", onFocus)');
  });

  it("tears the poll down on unmount (closing the panel or switching tabs)", () => {
    expect(viewSource).toContain("const interval = setInterval(");
    expect(viewSource).toContain("clearInterval(interval);");
    expect(viewSource).toContain('window.removeEventListener("focus", onFocus)');
    expect(viewSource).toContain('document.removeEventListener("visibilitychange", onFocus)');
  });
});
