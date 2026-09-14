// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * GitView merge-prompt contract test.
 *
 * The Git tab's per-branch "Merge to main" action does NOT use the
 * `merge_to_main` skill's own prompt — it overrides it with `mergePrompt`,
 * so the flow lives in two copies (`.coding/skills/merge_to_main.toml` and
 * here) that can drift apart.
 *
 * Pinned contract: `main` is synced with `origin` BEFORE the branch is
 * merged in. On 2026-09-13 the missing sync cost a separate catch-up
 * session — the branch landed on a stale `main` and the push was rejected
 * until `git fetch origin` + `git pull --no-rebase` ran. Ordering is the
 * point: a pull AFTER `git merge --no-ff` cannot prevent that.
 */

import { describe, expect, it } from "vitest";

import { mergePrompt } from "./GitView";

describe("mergePrompt — sync main with origin before the branch merge", () => {
  it("fetches and pulls origin (no rebase) before merging the branch", () => {
    const prompt = mergePrompt("wt/demo");
    const fetchAt = prompt.indexOf("git fetch origin");
    const pullAt = prompt.indexOf("git pull --no-rebase");
    const mergeAt = prompt.indexOf("git merge --no-ff wt/demo");

    // Both halves of the sync are present …
    expect(fetchAt).toBeGreaterThan(-1);
    expect(pullAt).toBeGreaterThan(-1);
    // … the branch merge is present …
    expect(mergeAt).toBeGreaterThan(-1);
    // … and the sync precedes it (test at -1 would pass a naive compare).
    expect(fetchAt).toBeLessThan(mergeAt);
    expect(pullAt).toBeLessThan(mergeAt);
  });

  it("keeps the branch out of its own merge target prompt", () => {
    // The named branch is interpolated everywhere it is needed, so the
    // Git tab can merge ANY branch (not just the checked-out one).
    const prompt = mergePrompt("wt/other");
    expect(prompt).toContain("git merge --no-ff wt/other");
    expect(prompt).not.toContain("git merge --no-ff wt/demo");
  });
});
