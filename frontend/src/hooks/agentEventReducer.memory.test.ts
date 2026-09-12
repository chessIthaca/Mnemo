// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Regression pin for `parseMemoryEntry` (the `memory` transcript-entry
 * parser) against the REAL memory tool output formats. The 2026-11-22 review
 * (finding C1) reported the recall regex never matching the actual output;
 * this suite pins the parser to the exact shapes `src/tool/memory/mod.rs`
 * emits today, so a format drift on the Rust side or a parser regression on
 * the TS side fails loudly here. Node environment — pure function, no DOM.
 */
import { describe, expect, it } from "vitest";
import { parseMemoryEntry } from "./agentEventReducer";

describe("parseMemoryEntry", () => {
  it("parses memory_search output (real format: [tier] title (id: …, score: x, strength: y))", () => {
    // Exact shape from src/tool/memory/mod.rs (Phase 1, 2026-08-22):
    // "[{tier}] {title} (id: {id}, score: {:.2}, strength: {:.2})[ [superseded]]\n  {content ≤ 200 chars}\n\n"
    // The stable id is the FIRST parenthesized field so the agent can address
    // memories straight from recall output.
    const output =
      "2 memories matched:\n\n" +
      "[semantic] merge instructions (id: 3f6a1b2c-9d4e-4f01-a2b3-c4d5e6f70819, score: 0.73, strength: 1.00)\n  how to merge feature branches into main safely\n\n" +
      "[procedural] closing sequence (id: 8c7d6e5f-4a3b-2c1d-0e9f-8a7b6c5d4e3f, score: 0.71, strength: 0.95) [superseded]\n  test, review, fix, commit, finish\n\n";
    const r = parseMemoryEntry("memory_search", { success: true, output });
    expect(r.tier).toBe("semantic");
    expect(r.title).toBe("merge instructions");
    expect(r.snippet).toBe("how to merge feature branches into main safely");
    // The header count ("2 memories matched:") drives the card's "N
    // matched" chip.
    expect(r.matched).toBe(2);
  });

  it("parses legacy memory_search output without ids ([tier] title (score: x, strength: y))", () => {
    // Pre-Phase-1 shape (older logs / older binaries): no id field. The
    // optional id group keeps these parsing.
    const output =
      "1 memories matched:\n\n" +
      "[semantic] merge instructions (score: 0.73, strength: 1.00)\n  how to merge feature branches into main safely\n\n";
    const r = parseMemoryEntry("memory_search", { success: true, output });
    expect(r.tier).toBe("semantic");
    expect(r.title).toBe("merge instructions");
    expect(r.snippet).toBe("how to merge feature branches into main safely");
    expect(r.matched).toBe(1);
  });

  it("parses record_type-scoped searches the same way (one tool, one line shape)", () => {
    // The typed-search tools emit the same recall line shape with their own
    // header noun ("N plans matched:" / "N reviews matched:" / "N past fixes
    // matched:") whatever record_type scoped them — one branch parses all.
    const plans = parseMemoryEntry("memory_search", {
      success: true,
      output:
        "2 plans matched:\n\n" +
        "[semantic] PLAN: storage migration (id: aaa111, score: 0.80)\n  migrate storage to sqlite with a plan file\n\n",
    });
    expect(plans.tier).toBe("semantic");
    expect(plans.title).toBe("PLAN: storage migration");
    expect(plans.snippet).toBe("migrate storage to sqlite with a plan file");
    expect(plans.matched).toBe(2);

    const reviews = parseMemoryEntry("memory_search", {
      success: true,
      output:
        "1 reviews matched:\n\n" +
        "[semantic] REVIEW: storage review (id: bbb222, score: 0.70)\n  review verdict: pass, 2 findings\n\n",
    });
    expect(reviews.title).toBe("REVIEW: storage review");

    const fixes = parseMemoryEntry("memory_search", {
      success: true,
      output:
        "1 past fixes matched:\n\n" +
        "[semantic] BUG: storage crash (id: ccc333, score: 0.90)\n  root cause: unwrap on None\n\n",
    });
    expect(fixes.title).toBe("BUG: storage crash");
    expect(fixes.snippet).toBe("root cause: unwrap on None");
  });

  it("parses the matched count for zero-match and header-less outputs", () => {
    // "no memories matched" (any typed noun) → 0 matches; output with no
    // matched-header (e.g. an error string) → undefined — the card hides
    // the count chip in that case.
    const none = parseMemoryEntry("memory_search", {
      success: true,
      output: "no memories matched",
    });
    expect(none.matched).toBe(0);
    const err = parseMemoryEntry("memory_search", {
      success: false,
      output: "error: store not open",
    });
    expect(err.matched).toBeUndefined();
  });

  it("parses memory_write confirmation (real format: Saved {phrase} \"title\" ({tier} memory) — …)", () => {
    // Exact shape from src/tool/memory/mod.rs (backlog 2026-08-20): a human
    // sentence naming what was saved; the write id rides in structured data,
    // not the text.
    const r = parseMemoryEntry("memory_write", {
      success: true,
      output:
        "Saved a durable fact \"auth fact\" (semantic memory) — it will be recalled in future sessions when relevant",
    });
    expect(r.tier).toBe("semantic");
    expect(r.title).toBe("auth fact");
    expect(r.snippet).toContain("Saved a durable fact");
  });

  it("parses the legacy memory_write confirmation (wrote {tier} memory (id: …))", () => {
    // Old binaries / old logs — the tier still parses via the fallback.
    const r = parseMemoryEntry("memory_write", {
      success: true,
      output: "wrote semantic memory (id: abc-123)",
    });
    expect(r.tier).toBe("semantic");
    expect(r.title).toBeUndefined();
    expect(r.snippet).toContain("wrote semantic memory");
  });

  it("parses a title containing double quotes (review L2 pin)", () => {
    // The title is embedded verbatim between quotes; the parser's greedy
    // capture runs to the LAST `" (tier memory)` marker, so inner quotes
    // don't truncate the title. (A first-quote regex captured "say " here.)
    const r = parseMemoryEntry("memory_write", {
      success: true,
      output:
        'Saved a durable fact "say "hi" now" (semantic memory) — it will be recalled in future sessions when relevant',
    });
    expect(r.tier).toBe("semantic");
    expect(r.title).toBe('say "hi" now');
  });

  it("parses a title containing a fake (tier memory) fragment (review L2 pin)", () => {
    // A tier-phrase lookalike INSIDE the title must not win the tier badge:
    // the marker is only valid when preceded by `" ` and followed by
    // ` — ` or end-of-string.
    const r = parseMemoryEntry("memory_write", {
      success: true,
      output:
        "Saved a durable fact \"weird (procedural memory) thing\" (semantic memory) — it will be recalled in future sessions when relevant",
    });
    expect(r.tier).toBe("semantic");
    expect(r.title).toBe("weird (procedural memory) thing");
  });

  it("parses the adversarial title ending in a fake marker (review L2 pin)", () => {
    // Strongest case: the title itself ends with `" (procedural memory)`.
    // The rightmost valid marker still wins → semantic, full title.
    const r = parseMemoryEntry("memory_write", {
      success: true,
      output:
        'Saved a durable fact "evil" (procedural memory)" (semantic memory) — it will be recalled in future sessions when relevant',
    });
    expect(r.tier).toBe("semantic");
    expect(r.title).toBe('evil" (procedural memory)');
  });

  it("parses the episodic form with no suffix (end-of-string marker)", () => {
    const r = parseMemoryEntry("memory_write", {
      success: true,
      output: 'Saved a session summary "wrap-up" (episodic memory)',
    });
    expect(r.tier).toBe("episodic");
    expect(r.title).toBe("wrap-up");
  });

  it("uses the memory_consolidate summary line as the snippet", () => {
    const r = parseMemoryEntry("memory_consolidate", {
      success: true,
      output: "compressed 5 working events into 1 episodic summary",
    });
    expect(r.snippet).toContain("compressed 5 working events");
  });

  it("an empty recall result yields no tier/title", () => {
    const r = parseMemoryEntry("memory_search", {
      success: true,
      output: "no memories matched the query",
    });
    expect(r.tier).toBeUndefined();
    expect(r.title).toBeUndefined();
  });

  it("parses the FULL hit list for the expandable memory_search card (backlog 41f39672)", () => {
    // Real 2-hit output (exact mod.rs shape): every [tier] row becomes a
    // hit with its tier, title and score — the card expands to all of them,
    // not just the first line the headline parser matches.
    const output =
      "2 memories matched:\n\n" +
      "[semantic] merge instructions (id: 3f6a1b2c-9d4e-4f01-a2b3-c4d5e6f70819, score: 0.73, strength: 1.00)\n  how to merge feature branches into main safely\n\n" +
      "[procedural] closing sequence (id: 8c7d6e5f-4a3b-2c1d-0e9f-8a7b6c5d4e3f, score: 0.71, strength: 0.95) [superseded]\n  test, review, fix, commit, finish\n\n";
    const r = parseMemoryEntry("memory_search", { success: true, output });
    expect(r.hits).toEqual([
      { tier: "semantic", title: "merge instructions", score: 0.73 },
      { tier: "procedural", title: "closing sequence", score: 0.71 },
    ]);
  });

  it("browse-mode results populate the card (backlog aec6cc8a — bare rows)", () => {
    // The BROWSE emitter shape (retrieval.rs, no query): header
    // "N memories (newest first):" + rows "{id}  {tier}  {record_type}
    // {date}  {title}[ [superseded]]" — no brackets, no score. These
    // cards used to render completely bare (no tier/title/count/hits),
    // leaving "found 10" indistinguishable from "found nothing".
    const rows: string[] = [];
    for (let i = 0; i < 10; i++) {
      rows.push(
        `3f6a1b2c-9d4e-4f01-a2b3-c4d5e6f7081${i}  semantic  plan  2026-09-16  PLAN: digest ${i}`,
      );
    }
    rows.push(
      "8c7d6e5f-4a3b-2c1d-0e9f-8a7b6c5d4e3f  procedural  how  2026-08-29  HOW: checklist [superseded]",
    );
    const r = parseMemoryEntry("memory_search", {
      success: true,
      output: "11 memories (newest first):\n\n" + rows.join("\n") + "\n",
    });
    expect(r.matched).toBe(11);
    expect(r.tier).toBe("semantic");
    expect(r.title).toBe("PLAN: digest 0");
    expect(r.hits).toHaveLength(11);
    // Score is undefined for browse rows — the card renders it only when
    // present.
    expect(r.hits?.[0]?.score).toBeUndefined();
  });

  it("zero-hit browse says no matches (backlog aec6cc8a)", () => {
    const r = parseMemoryEntry("memory_search", {
      success: true,
      output: "no memories match the filter",
    });
    expect(r.matched).toBe(0);
    expect(r.hits).toEqual([]);
  });

  it("zero-hit and non-search results carry no hits list", () => {
    // "no memories matched" → defined matched=0 but NO hits array; a
    // memory_write result never has hits. The card stays non-expandable in
    // both cases.
    const none = parseMemoryEntry("memory_search", {
      success: true,
      output: "no memories matched",
    });
    expect(none.matched).toBe(0);
    expect(none.hits).toEqual([]);
    const written = parseMemoryEntry("memory_write", {
      success: true,
      output: 'Saved a durable fact "auth fact" (semantic memory) — suffix',
    });
    // The memory_write branch returns before the hits parsing — the key is
    // absent entirely → the card stays non-expandable.
    expect(written.hits).toBeUndefined();
  });
});
