// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Transcript image byte budget (mem-perf review LOW 4): the entry-count cap
 * alone let full-size pasted screenshots accumulate in the zustand store
 * AND the DOM for the whole session. `capTranscriptImages` bounds the
 * retained image payload with newest-first retention — the overflow entry
 * and every older image-bearing entry is evicted to an `imagesEvicted`
 * count (Message renders a placeholder chip). Pure, deterministic unit
 * tests; the function is folded into `capTranscript`, so every append site
 * (direct pushes, reducer events, /load restores) enforces the budget.
 */

import { describe, expect, it } from "vitest";
import {
  MAX_TRANSCRIPT_ENTRIES,
  MAX_TRANSCRIPT_IMAGE_CHARS,
  capTranscript,
  capTranscriptImages,
} from "./agentState";
import type { TranscriptEntry } from "../lib/types";

/** A user entry with optional image attachments. */
function userEntry(text: string, images?: string[]): TranscriptEntry {
  return images ? { kind: "user", text, images } : { kind: "user", text };
}

/**
 * A fake image payload of exactly `chars` characters — only `.length` is
 * summed by the budget walk, the content is irrelevant.
 */
function imageChars(chars: number): string {
  return "A".repeat(chars);
}

describe("capTranscriptImages", () => {
  it("returns the same array when under budget", () => {
    const entries: TranscriptEntry[] = [
      userEntry("old", [imageChars(1024)]),
      { kind: "assistant", text: "hi" },
      userEntry("new", [imageChars(2048)]),
    ];
    expect(capTranscriptImages(entries)).toBe(entries);
  });

  it("returns the same array when there are no images at all", () => {
    const entries: TranscriptEntry[] = [
      userEntry("a"),
      { kind: "assistant", text: "b" },
    ];
    expect(capTranscriptImages(entries)).toBe(entries);
  });

  it("keeps entries at exactly the budget (boundary: no eviction)", () => {
    const half = Math.floor(MAX_TRANSCRIPT_IMAGE_CHARS / 2);
    const entries = [
      userEntry("a", [imageChars(half)]),
      userEntry("b", [imageChars(MAX_TRANSCRIPT_IMAGE_CHARS - half)]),
    ];
    expect(capTranscriptImages(entries)).toBe(entries);
  });

  it("evicts the oldest images when over budget, keeping the newest", () => {
    // The oldest entry's images alone exceed the budget.
    const old = userEntry("old", [imageChars(MAX_TRANSCRIPT_IMAGE_CHARS)]);
    const mid: TranscriptEntry = { kind: "assistant", text: "mid" };
    const newest = userEntry("new", [imageChars(100)]);
    const result = capTranscriptImages([old, mid, newest]);

    // Evicted to a placeholder count — payload gone.
    expect(result[0]).toEqual({ kind: "user", text: "old", imagesEvicted: 1 });
    // Non-user entries keep their object identity.
    expect(result[1]).toBe(mid);
    // The newest entry's images are retained untouched.
    expect(result[2]).toBe(newest);
  });

  it("evicts every image-bearing entry older than the overflow point", () => {
    // Budget fits only the newest entry; the middle entry overflows, so it
    // AND the oldest are evicted.
    const a = userEntry("a", [imageChars(MAX_TRANSCRIPT_IMAGE_CHARS)]);
    const b = userEntry("b", [imageChars(MAX_TRANSCRIPT_IMAGE_CHARS)]);
    const c = userEntry("c", [imageChars(50)]);
    const result = capTranscriptImages([a, b, c]);
    expect(result[0]).toEqual({ kind: "user", text: "a", imagesEvicted: 1 });
    expect(result[1]).toEqual({ kind: "user", text: "b", imagesEvicted: 1 });
    expect(result[2]).toBe(c);
  });

  it("retains middle entries that still fit (partial retention)", () => {
    // One char over budget in total: the newest two fit, only the oldest
    // is evicted — eviction is per-entry, newest-first.
    const half = Math.floor(MAX_TRANSCRIPT_IMAGE_CHARS / 2);
    const a = userEntry("a", [imageChars(half)]);
    const b = userEntry("b", [imageChars(MAX_TRANSCRIPT_IMAGE_CHARS - half)]);
    const c = userEntry("c", [imageChars(1)]);
    const result = capTranscriptImages([a, b, c]);
    expect(result[0]).toEqual({ kind: "user", text: "a", imagesEvicted: 1 });
    expect(result[1]).toBe(b);
    expect(result[2]).toBe(c);
  });

  it("evicts a single entry whose images alone exceed the budget", () => {
    const only = userEntry("only", [
      imageChars(MAX_TRANSCRIPT_IMAGE_CHARS + 1),
    ]);
    const result = capTranscriptImages([only]);
    expect(result[0]).toEqual({
      kind: "user",
      text: "only",
      imagesEvicted: 1,
    });
  });

  it("counts every image of a multi-image entry in the placeholder", () => {
    // Two images in one entry: the second overflows the budget, so the
    // whole entry is evicted with imagesEvicted: 2 (not 1).
    const entry = userEntry("multi", [
      imageChars(MAX_TRANSCRIPT_IMAGE_CHARS),
      imageChars(10),
    ]);
    const result = capTranscriptImages([entry]);
    expect(result[0]).toEqual({
      kind: "user",
      text: "multi",
      imagesEvicted: 2,
    });
  });

  it("is idempotent — re-running on its own output changes nothing", () => {
    const a = userEntry("a", [imageChars(MAX_TRANSCRIPT_IMAGE_CHARS)]);
    const b = userEntry("b", [imageChars(10)]);
    const once = capTranscriptImages([a, b]);
    expect(capTranscriptImages(once)).toBe(once);
  });

  it("never touches empty images arrays (no payload, no placeholder)", () => {
    const empty = userEntry("empty", []);
    const big = userEntry("big", [
      imageChars(MAX_TRANSCRIPT_IMAGE_CHARS + 1),
    ]);
    const result = capTranscriptImages([empty, big]);
    expect(result[0]).toBe(empty);
    const first = result[0];
    if (first.kind === "user") {
      expect(first.imagesEvicted).toBeUndefined();
      expect(first.images).toEqual([]);
    }
    expect(result[1]).toEqual({ kind: "user", text: "big", imagesEvicted: 1 });
  });

  it("preserves sibling fields (text, ts, entryId) on evicted entries", () => {
    const a: TranscriptEntry = {
      kind: "user",
      text: "a",
      images: [imageChars(MAX_TRANSCRIPT_IMAGE_CHARS + 1)],
      ts: 123,
      entryId: 45,
    };
    const result = capTranscriptImages([a]);
    expect(result[0]).toEqual({
      kind: "user",
      text: "a",
      imagesEvicted: 1,
      ts: 123,
      entryId: 45,
    });
  });
});

describe("capTranscript (count cap + image budget composition)", () => {
  it("is a no-op (same reference) under both caps", () => {
    const entries = [userEntry("a", [imageChars(10)])];
    expect(capTranscript(entries)).toBe(entries);
  });

  it("composes the count cap with the image budget", () => {
    // 1001 entries: the oldest is dropped by the count cap; the next one's
    // images alone exceed the budget and are evicted.
    const entries: TranscriptEntry[] = [];
    entries.push(userEntry("dropped", [imageChars(10)]));
    entries.push(
      userEntry("oversize", [imageChars(MAX_TRANSCRIPT_IMAGE_CHARS + 1)]),
    );
    for (let i = 0; i < 999; i++) {
      entries.push({ kind: "assistant", text: `t${i}` });
    }
    expect(entries.length).toBe(MAX_TRANSCRIPT_ENTRIES + 1);

    const result = capTranscript(entries);
    expect(result.length).toBe(MAX_TRANSCRIPT_ENTRIES);
    // The count-dropped oldest is gone entirely.
    expect(
      result.some((e) => e.kind === "user" && e.text === "dropped"),
    ).toBe(false);
    // The oversize entry survived the count cap but was image-evicted.
    expect(result[0]).toEqual({
      kind: "user",
      text: "oversize",
      imagesEvicted: 1,
    });
  });
});
