// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect } from "vitest";
import {
  buildGraphModel,
  clampBottomHeight,
  formatAccessLine,
  isGraphUnavailable,
  kindColor,
  shouldReloadOnIndexingEdge,
} from "./GraphView";
import source from "./GraphView.tsx?raw";
import type {
  CodegraphStatus,
  GraphEdge,
  GraphSymbol,
  MemoryAccessEntry,
} from "../../lib/tauri";

/**
 * Unit tests for the Graph tab's pure render-model helpers (the repo's
 * component-test pattern: extract pure logic, test it directly — no DOM
 * rendering).
 */

function sym(id: string, name: string, kind = "function"): GraphSymbol {
  return { id, name, kind, file: "src/x.rs", start_line: 1, end_line: 5 };
}

function edge(from: string, to: string, kind = "calls"): GraphEdge {
  return { from_id: from, to_id: to, kind };
}

describe("buildGraphModel", () => {
  it("resolves links to node indexes and counts degrees (in + out)", () => {
    const nodes = [sym("a", "fa"), sym("b", "fb"), sym("c", "fc")];
    const edges = [
      edge("a", "b"), // a→b
      edge("c", "b"), // c→b
    ];
    const m = buildGraphModel(nodes, edges);
    expect(m.links).toEqual([
      { from: 0, to: 1, kind: "calls" },
      { from: 2, to: 1, kind: "calls" },
    ]);
    expect(m.degrees).toEqual([1, 2, 1]);
  });

  it("drops edges whose endpoints are not in the payload (dangling ids)", () => {
    const nodes = [sym("a", "fa")];
    const edges = [
      edge("a", "gone"), // target not in nodes
      edge("ghost", "a"), // source not in nodes
    ];
    const m = buildGraphModel(nodes, edges);
    expect(m.links).toEqual([]);
    expect(m.degrees).toEqual([0]);
  });

  it("radii grow with degree, capped at 14", () => {
    // Hub with 100 in-edges → capped; isolated node → base radius 4.
    const hub = sym("hub", "hub");
    const nodes: GraphSymbol[] = [hub];
    const edges: GraphEdge[] = [];
    for (let i = 0; i < 100; i++) {
      const leaf = sym(`l${i}`, `fl${i}`);
      nodes.push(leaf);
      edges.push(edge(`l${i}`, "hub"));
    }
    const m = buildGraphModel(nodes, edges);
    expect(m.radii[0]).toBe(14); // 4 + √100 = 14 exactly at the cap
    // One more edge would exceed the cap.
    edges.push(edge("l0", "hub"));
    const m2 = buildGraphModel(nodes, edges);
    expect(m2.radii[0]).toBe(14);
    expect(m2.radii[1]).toBeLessThanOrEqual(14);
    // Isolated node gets the base radius.
    const iso = buildGraphModel([sym("solo", "solo")], []);
    expect(iso.radii[0]).toBe(4);
  });
});

describe("kindColor", () => {
  it("maps known kinds to their legend colors and unknowns to neutral", () => {
    expect(kindColor("function")).toBe("#38bdf8");
    expect(kindColor("struct")).toBe("#34d399");
    // Unknown kinds must not crash and get a neutral color.
    expect(kindColor("martian")).toBe("#6b7280");
  });
});

describe("shouldReloadOnIndexingEdge", () => {
  it("reloads only on the true→false falling edge (review F1 regression)", () => {
    // A pass just completed → the store changed under the tab → reload.
    expect(shouldReloadOnIndexingEdge(true, false)).toBe(true);
    // No transition, or a pass starting, must NOT reload: false→false
    // (steady state / mount race case), false→true (pass started — the poll
    // cadence + eventual falling edge handle it), true→true (still running).
    expect(shouldReloadOnIndexingEdge(false, false)).toBe(false);
    expect(shouldReloadOnIndexingEdge(false, true)).toBe(false);
    expect(shouldReloadOnIndexingEdge(true, true)).toBe(false);
  });
});

describe("formatAccessLine", () => {
  function entry(overrides: Partial<MemoryAccessEntry>): MemoryAccessEntry {
    return { at: 1700000000, op: "read", tier: null, detail: "d", hits: null, ...overrides };
  }

  it("read with tier + hit count", () => {
    expect(
      formatAccessLine(entry({ op: "read", tier: "semantic", detail: "login route", hits: 3 })),
    ).toBe('R [semantic] "login route" → 3 hits');
  });

  it("read with null hits omits the suffix", () => {
    expect(formatAccessLine(entry({ op: "read", detail: "anything" }))).toBe('R "anything"');
  });

  it("read with null tier omits the bracket", () => {
    expect(formatAccessLine(entry({ op: "read", hits: 0 }))).toBe('R "d" → 0 hits');
  });

  it("write with tier", () => {
    expect(
      formatAccessLine(entry({ op: "write", tier: "working", detail: "tool: search" })),
    ).toBe('W [working] "tool: search"');
  });

  it("write with null tier omits the bracket, and never shows a hit count", () => {
    // Even a stray hits value on a write must not render (writes carry none).
    expect(formatAccessLine(entry({ op: "write", hits: 5 }))).toBe('W "d"');
  });
});

describe("isGraphUnavailable", () => {
  // Review B1 regression pin: the unavailable early-return branch must keep
  // rendering the memory-access section (memory data is independent of
  // codegraph availability). This predicate is what selects that branch —
  // true ONLY for a loaded-but-unavailable status, so the loading state
  // (null) renders the full explorer tree (section included) and only the
  // unavailable state takes the notice + section layout.
  function status(available: boolean): CodegraphStatus {
    return {
      available,
      indexing: false,
      files: 0,
      symbols: 0,
      edges: 0,
      last_indexed_at: null,
    };
  }

  it("true only for a loaded-but-unavailable status", () => {
    expect(isGraphUnavailable(null)).toBe(false);
    expect(isGraphUnavailable(status(true))).toBe(false);
    expect(isGraphUnavailable(status(false))).toBe(true);
  });
});

describe("clampBottomHeight", () => {
  it("never shrinks below the 80px minimum", () => {
    expect(clampBottomHeight(0, 1000)).toBe(80);
    expect(clampBottomHeight(20, 1000)).toBe(80);
    // Even a negative drag far past the top edge.
    expect(clampBottomHeight(-300, 1000)).toBe(80);
  });

  it("never exceeds 60% of the tab height", () => {
    expect(clampBottomHeight(900, 1000)).toBe(600);
    expect(clampBottomHeight(10_000, 1000)).toBe(600);
  });

  it("passes in-range heights through, rounded", () => {
    expect(clampBottomHeight(200, 1000)).toBe(200);
    expect(clampBottomHeight(200.6, 1000)).toBe(201);
  });
});

describe("Graph tab browse-to-source + resizable bottom pane (source contract)", () => {
  // This project has no React DOM test infra (vitest runs in `node`
  // environment), so the wiring below is pinned statically in the style of
  // `src/lib/ipc-contract.test.ts` (Vite `?raw` import).

  it("the selected node offers a Browse source action", () => {
    expect(source).toContain("Browse source");
  });

  it("the browsed file renders through the SHARED SourceEditor with its line", () => {
    // Not a copy: the same component the Files tab renders, with the
    // symbol's start_line wired to revealLine and a ✕ back to the log.
    expect(source).toContain("SourceEditor");
    expect(source).toContain("revealLine={browse.line}");
    expect(source).toContain("onClose={() => setBrowse(null)}");
  });

  it("the bottom area is drag-to-resizable (splitter contract)", () => {
    expect(source).toContain('aria-label="Resize bottom area"');
    expect(source).toContain("cursor-row-resize");
  });

  it("the bottom height persists across sessions", () => {
    expect(source).toContain("graphview.bottomHeight");
    expect(source).toContain("localStorage.setItem(BOTTOM_HEIGHT_KEY");
  });

  it("the memory-access log renders in BOTH layout branches (review B1)", () => {
    const occurrences = source.match(/<MemoryAccessSection log=\{accessLog\} \/>/g) ?? [];
    expect(occurrences.length).toBe(2);
  });

  it("the unavailable branch's log pane is height-bounded (review L1 regression)", () => {
    // h-full inside an auto-height parent renders unbounded (100 entries ≈
    // 1400px, clipped) — BOTH panes must carry the explicit persisted
    // height.
    const bounded =
      source.split("style={{ height: `${bottomHeight}px` }}").length - 1;
    expect(bounded).toBe(2);
  });

  it("the persisted height is re-clamped on mount (review N2 regression)", () => {
    // A height saved on a large window must not render oversized on a
    // smaller one before the first drag.
    expect(source).toContain("setBottomHeight((h) => clampBottomHeight(h, total))");
  });
});
