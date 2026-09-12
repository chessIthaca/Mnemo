// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect } from "vitest";
import { shouldFlushSync, shouldFlushNow, MAX_FLUSH_INTERVAL_MS } from "./deltaFlush";

describe("deltaFlush policy predicates", () => {
  it("MAX_FLUSH_INTERVAL_MS is 40ms — the time-gap immediate-flush threshold", () => {
    expect(MAX_FLUSH_INTERVAL_MS).toBe(40);
  });

  it("shouldFlushNow: true exactly at the threshold", () => {
    expect(shouldFlushNow(1000, 1000 + MAX_FLUSH_INTERVAL_MS)).toBe(true);
  });

  it("shouldFlushNow: false just inside the threshold (dense burst batches)", () => {
    expect(shouldFlushNow(1000, 1000 + MAX_FLUSH_INTERVAL_MS - 1)).toBe(false);
  });

  it("shouldFlushNow: true for a sparse gap (slow reasoning-model tokens)", () => {
    expect(shouldFlushNow(1000, 1500)).toBe(true);
  });

  it("shouldFlushNow: false when lastFlushAt is in the future (clock skew guard)", () => {
    expect(shouldFlushNow(2000, 1500)).toBe(false);
  });

  it("shouldFlushSync: the 64 KiB occlusion cap is unchanged", () => {
    expect(shouldFlushSync(64 * 1024 - 1)).toBe(false);
    expect(shouldFlushSync(64 * 1024)).toBe(true);
  });
});
