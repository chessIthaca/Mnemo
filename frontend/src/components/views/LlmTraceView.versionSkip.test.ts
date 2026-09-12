// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * LlmTraceView detail-fetch version-skip contract test (perf review L5,
 * 2027-01-09, .coding/reviews/2026-09-09-perf-comm-review.md).
 *
 * The detail/compare polls re-fetch the FULL record every 1.5s — up to
 * ~2 MiB of JSON (raw response cap + capped request body) per tick while a
 * request streams. The fix: the trace record carries a mutation `version`
 * (bumped on every mutation and payload eviction), the cheap list poll
 * carries it, and each poll skips the fetch while the selected record's
 * version is unchanged since its last fetch.
 *
 * Two layers, in the ModelsSection style:
 *  - the pure predicate `shouldSkipRefetch` is table-tested directly;
 *  - the wiring (guard BEFORE each fetch, version captured from each fetch,
 *    list poll feeding the ref) is pinned via Vite's `?raw` source import —
 *    this project has no React DOM test infra (vitest runs in `node`), so
 *    the wiring is asserted statically in the style of ipc-contract.test.ts.
 */

import { describe, expect, it } from "vitest";
import { shouldSkipRefetch } from "./LlmTraceView";
import source from "./LlmTraceView.tsx?raw";

describe("shouldSkipRefetch (L5 regression)", () => {
  it("skips when the record's version is unchanged since the last fetch", () => {
    expect(shouldSkipRefetch(3, 3)).toBe(true);
    expect(shouldSkipRefetch(0, 0)).toBe(true);
  });

  it("fetches when the version changed (a mutation landed)", () => {
    expect(shouldSkipRefetch(3, 4)).toBe(false);
    expect(shouldSkipRefetch(4, 3)).toBe(false);
  });

  it("fetches before the first detail lands (lastVersion null)", () => {
    // No detail yet / the selection just changed — the effect-scoped
    // lastVersion resets to null and the first fetch must always run.
    expect(shouldSkipRefetch(null, 3)).toBe(false);
  });

  it("fetches when the record is absent from the list (listVersion null)", () => {
    // Never recorded / ring-evicted: the fetch must run so the poll can
    // observe the null and stop (shouldStopPolling) — skipping here would
    // keep the poll alive forever without ever learning the record is gone.
    expect(shouldSkipRefetch(3, null)).toBe(false);
    expect(shouldSkipRefetch(null, null)).toBe(false);
  });
});

describe("LlmTraceView version-skip wiring (source contract)", () => {
  const guard = "if (shouldSkipRefetch(lastVersion, listVersion)) return;";
  const detailFetch = "getLlmRequest(selectedId)";
  const compareFetch = "getLlmRequest(selectedId - 1)";

  it("guards BOTH polls: the skip runs before each getLlmRequest call", () => {
    // The guard must PREVENT the fetch, not merely ignore its result —
    // each ~2 MiB fetch is the thing being skipped. The detail load's guard
    // precedes the detail fetch; the compare load's guard precedes the
    // compare fetch.
    const firstGuard = source.indexOf(guard);
    const secondGuard = source.indexOf(guard, firstGuard + 1);
    expect(firstGuard).toBeGreaterThan(-1);
    expect(secondGuard).toBeGreaterThan(firstGuard);
    expect(source.indexOf(detailFetch)).toBeGreaterThan(firstGuard);
    expect(source.indexOf(compareFetch)).toBeGreaterThan(secondGuard);
  });

  it("captures the fetched record's version into lastVersion in both loads", () => {
    // Without this the guard would compare against a stale version and
    // either never skip or skip a record that changed.
    expect(source).toContain("lastVersion = d === null ? null : d.version;");
    expect(source).toContain("lastVersion = p === null ? null : p.version;");
  });

  it("reads the version from the latest list poll via requestsRef", () => {
    // The list poll is the cheap carrier of the mutation version — both
    // loads must consult it (detail: selectedId, compare: selectedId - 1).
    expect(source).toContain("listVersionOf(requestsRef.current, selectedId)");
    expect(source).toContain("listVersionOf(requestsRef.current, selectedId - 1)");
  });

  it("feeds requestsRef beside EVERY setRequests call (list poll + clear)", () => {
    // The ref must be updated wherever the requests state is (kept in
    // lockstep) or the skip would compare against a stale version. There are
    // exactly two setRequests call sites — the list poll tick and the clear
    // handler — and each must pair its state update with a ref update
    // (review round-1, Finding 1: the clear handler originally drifted).
    expect(source).toContain("const requestsRef = useRef<LlmRequestSummary[]>([]);");
    expect(source).toContain("requestsRef.current = list;");
    expect(source).toContain("requestsRef.current = [];");
    expect(source.match(/setRequests\(/g)?.length).toBe(2);
    expect(source.match(/requestsRef\.current = /g)?.length).toBe(2);
  });

  it("resets lastVersion per effect run (effect-scoped, not a ref)", () => {
    // A re-select / compare re-toggle must always re-fetch: lastVersion is
    // declared with `let` INSIDE each effect, so it restarts at null.
    expect(source).toContain("let lastVersion: number | null = null;");
    expect(source.match(/let lastVersion: number \| null = null;/g)?.length).toBe(2);
  });
});
