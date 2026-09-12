// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";

import { arePropsEqual } from "./messageEquality";
import type { TranscriptEntry, ToolInvocation } from "./types";

/**
 * Behavior tests for the Message component's memo-equality (moved out of
 * Message.tsx by quality review LOW 2 — the old coverage was a source-
 * contract string check, which can't catch logic regressions). The
 * contract: finalized transcript entries never change, so equal props skip
 * re-renders; any semantic difference must re-render.
 */
describe("arePropsEqual", () => {
  it("streaming toggle re-renders (plain-text ↔ markdown switch)", () => {
    const entry: TranscriptEntry = { kind: "assistant", text: "hi" };
    expect(arePropsEqual({ entry }, { entry, streaming: true })).toBe(false);
    expect(arePropsEqual({ entry, streaming: true }, { entry })).toBe(false);
    expect(arePropsEqual({ entry, streaming: true }, { entry, streaming: true })).toBe(true);
  });

  it("kind mismatch re-renders", () => {
    expect(
      arePropsEqual(
        { entry: { kind: "assistant", text: "x" } },
        { entry: { kind: "error", text: "x" } },
      ),
    ).toBe(false);
  });

  it("text kinds: equal text skips, differing text re-renders", () => {
    for (const kind of ["assistant", "user", "error", "steer"] as const) {
      expect(arePropsEqual({ entry: { kind, text: "same" } }, { entry: { kind, text: "same" } })).toBe(true);
      expect(arePropsEqual({ entry: { kind, text: "a" } }, { entry: { kind, text: "b" } })).toBe(false);
    }
  });

  it("user images: reference inequality re-renders", () => {
    const images = ["data:image/png;base64,aaa"];
    expect(
      arePropsEqual(
        { entry: { kind: "user", text: "t", images } },
        { entry: { kind: "user", text: "t", images } },
      ),
    ).toBe(true);
    expect(
      arePropsEqual(
        { entry: { kind: "user", text: "t", images } },
        { entry: { kind: "user", text: "t", images: [...images] } },
      ),
    ).toBe(false);
  });

  it("user imagesEvicted: the image budget flipping an entry to evicted re-renders", () => {
    // The regression the old source-contract check guarded (mem-perf LOW 4):
    // capTranscriptImages replaces `images` with an `imagesEvicted` count —
    // the placeholder chip must render.
    expect(
      arePropsEqual(
        { entry: { kind: "user", text: "t", images: ["a"] } },
        { entry: { kind: "user", text: "t", imagesEvicted: 1 } },
      ),
    ).toBe(false);
    expect(
      arePropsEqual(
        { entry: { kind: "user", text: "t", imagesEvicted: 1 } },
        { entry: { kind: "user", text: "t", imagesEvicted: 2 } },
      ),
    ).toBe(false);
    expect(
      arePropsEqual(
        { entry: { kind: "user", text: "t", imagesEvicted: 1 } },
        { entry: { kind: "user", text: "t", imagesEvicted: 1 } },
      ),
    ).toBe(true);
  });

  it("tool: calls compared by reference — same array skips, new array re-renders", () => {
    const calls: ToolInvocation[] = [];
    expect(
      arePropsEqual(
        { entry: { kind: "tool", name: "shell", calls } },
        { entry: { kind: "tool", name: "shell", calls } },
      ),
    ).toBe(true);
    expect(
      arePropsEqual(
        { entry: { kind: "tool", name: "shell", calls } },
        { entry: { kind: "tool", name: "shell", calls: [...calls] } },
      ),
    ).toBe(false);
  });

  it("qa: question and answer both compared", () => {
    expect(
      arePropsEqual(
        { entry: { kind: "qa", question: "q", answer: "a" } },
        { entry: { kind: "qa", question: "q", answer: "a" } },
      ),
    ).toBe(true);
    expect(
      arePropsEqual(
        { entry: { kind: "qa", question: "q", answer: "a" } },
        { entry: { kind: "qa", question: "q", answer: "b" } },
      ),
    ).toBe(false);
    expect(
      arePropsEqual(
        { entry: { kind: "qa", question: "q", answer: "a" } },
        { entry: { kind: "qa", question: "q2", answer: "a" } },
      ),
    ).toBe(false);
  });

  it("skill: name and prompt compared", () => {
    expect(
      arePropsEqual(
        { entry: { kind: "skill", name: "s", prompt: "p" } },
        { entry: { kind: "skill", name: "s", prompt: "p" } },
      ),
    ).toBe(true);
    expect(
      arePropsEqual(
        { entry: { kind: "skill", name: "s", prompt: "p" } },
        { entry: { kind: "skill", name: "s2", prompt: "p" } },
      ),
    ).toBe(false);
  });

  it("memory: every displayed field compared (hits by reference)", () => {
    const base = {
      kind: "memory" as const,
      name: "memory_search",
      tier: "semantic",
      title: "t",
      snippet: "s",
      success: true,
      running: false,
    };
    expect(arePropsEqual({ entry: base }, { entry: { ...base } })).toBe(true);
    expect(arePropsEqual({ entry: base }, { entry: { ...base, title: "other" } })).toBe(false);
    expect(arePropsEqual({ entry: base }, { entry: { ...base, running: true } })).toBe(false);
    const hits = [{ tier: "semantic", title: "h" }];
    expect(arePropsEqual({ entry: { ...base, hits } }, { entry: { ...base, hits } })).toBe(true);
    expect(
      arePropsEqual({ entry: { ...base, hits } }, { entry: { ...base, hits: [...hits] } }),
    ).toBe(false);
  });

  it("vision: every displayed field compared", () => {
    const base = {
      kind: "vision" as const,
      index: 1,
      total: 2,
      query: "what is this",
      description: null,
      success: false,
      running: true,
    };
    expect(arePropsEqual({ entry: base }, { entry: { ...base } })).toBe(true);
    expect(
      arePropsEqual(
        { entry: base },
        { entry: { ...base, description: "a cat", running: false, success: true } },
      ),
    ).toBe(false);
    expect(arePropsEqual({ entry: base }, { entry: { ...base, index: 2 } })).toBe(false);
  });

  it("unknown kind re-renders (default case)", () => {
    expect(
      arePropsEqual(
        { entry: { kind: "mystery" } as unknown as TranscriptEntry },
        { entry: { kind: "mystery" } as unknown as TranscriptEntry },
      ),
    ).toBe(false);
  });
});
