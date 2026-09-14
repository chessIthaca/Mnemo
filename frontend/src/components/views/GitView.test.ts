// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * GitView merge-dispatch contract test.
 *
 * The Git tab's per-branch "Merge to main" action enters the `merge_to_main`
 * skill and sends ONE line naming the target branch (see `mergeDispatch`). The
 * procedure — commit-all, sync main with origin BEFORE the merge, `--no-ff`
 * merge, conflict resolution, both builds, branch cleanup — lives only in
 * `.coding/skills/merge_to_main.toml`, whose content and step order are pinned
 * by the Rust test
 * `skill::tests::shipped_skill_files_parse_and_merge_to_main_cleans_up_memories`.
 *
 * This file supplies the other half of that contract: the UI may name a target,
 * never restate the steps. The copy that used to live in GitView had already
 * drifted out of date — it still claimed a shell-invoked `git merge` skips the
 * approval prompt (false since `ShellTool::never_auto_for`, 2027-01-11), and it
 * permitted a stash the skill forbids.
 */

import { describe, expect, it } from "vitest";

import { mergeDispatch } from "./GitView";
import gitViewSource from "./GitView.tsx?raw";

describe("mergeDispatch — names the target, never the procedure", () => {
  it("is a single line naming the branch to merge into main", () => {
    const line = mergeDispatch("wt/demo");
    expect(line.split("\n")).toHaveLength(1);
    expect(line).toContain("wt/demo");
    expect(line).toContain("main");
  });

  it("carries no step text for any branch", () => {
    // Every step of the flow must come from the skill file, so a re-introduced
    // procedure copy fails here rather than silently shadowing it.
    for (const branch of ["wt/demo", "wt/other"]) {
      const line = mergeDispatch(branch);
      for (const step of [
        "Steps:",
        "git fetch",
        "git pull",
        "git checkout",
        "git merge",
        "--no-ff",
        "git add",
        "npm run build",
        "cargo build",
        "file_edit",
        "skill_end",
        "stash",
      ]) {
        expect(line).not.toContain(step);
      }
    }
  });

  it("is exactly what the click handler sends (source contract)", () => {
    // The tab cannot re-author the flow: the handler passes the dispatch line
    // and nothing else to enterSkill.
    expect(gitViewSource).toContain(
      'enterSkill(activeAgent, "merge_to_main", mergeDispatch(mergeTarget))',
    );
    expect(gitViewSource).not.toContain("mergePrompt");
  });
});
