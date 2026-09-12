// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * InputBar steer-bubble markdown wiring (plan 2026-12): queued steer
 * prompts render as inline markdown (bold/code, truncation-safe — never
 * block elements) matching the other read-only backlog surfaces. Static
 * source-contract test in the style of BacklogView.test.ts (vitest runs
 * in `node` env — no React DOM infra). The InlineMarkdown output contract
 * itself is covered in InlineMarkdown.test.ts.
 */

import { describe, expect, it } from "vitest";
import source from "./InputBar.tsx?raw";

describe("InputBar steer-bubble markdown rendering", () => {
  it("renders steer text through InlineMarkdown", () => {
    expect(source).toContain(
      'import { InlineMarkdown } from "../chat/InlineMarkdown";',
    );
    expect(source).toContain("<InlineMarkdown text={steer.text} />");
  });

  it("keeps the bubble single-line (truncate)", () => {
    // The bubble stays a one-line surface — CSS truncation, and
    // InlineMarkdown never emits block elements.
    expect(source).toContain('<span className="truncate">');
  });
});

describe("InputBar slash-command Enter behavior (regression: /compact + Enter never ran)", () => {
  it("runs fully-typed argument-less commands instead of re-completing", () => {
    // The menu-open Enter branch must consult resolveArglessCommand so
    // "/compact" and the completed "/compact " state run immediately
    // (pre-fix: Enter re-completed forever, only the Send button worked).
    expect(source).toContain("resolveArglessCommand(text)");
  });

  it("drives the immediate-run arm from takesArgument metadata", () => {
    // completeSlashCommand's immediate-run arm is data-driven
    // (!cmd.takesArgument) so every argument-less command — including
    // /compact and /new — runs on selection, not just the old hardcoded
    // clear|panel|help set.
    expect(source).toContain("!cmd.takesArgument");
  });
});

describe("InputBar mid-run slash-command routing (regression: menu-closed /compact steered instead of running)", () => {
  /**
   * Backlog b47b3f44 (review L1 of plan cb91d6ea, pre-existing): with the
   * slash menu Esc-dismissed (or any menu-closed state) and the agent
   * running, Enter/Send on an argument-less command (/compact, /new,
   * /clear) routed through handleSend's steerMode branch and was sent to
   * the model as a steer suggestion — while the menu-open path ran it
   * directly (completeSlashCommand → handleSlashCommand, bypassing steer
   * interception by design). The fix: handleSend's slash-command check
   * must precede the steer branch, so slash commands execute in every
   * agent state (running or idle) — they are control input, never steer.
   */
  const send = source.slice(
    source.indexOf("async function handleSend"),
    source.indexOf("async function handleStop"),
  );

  /**
   * The regression test for the mid-run asymmetry: the slash-command check
   * must sit BEFORE the steer branch inside handleSend. Pre-fix the order
   * was steer → slash, so a menu-closed "/compact" mid-run was steered to
   * the model instead of executed. A named function (not an inline arrow)
   * so the code graph indexes it as a symbol and the bug plan's
   * regression-test gate can point at the actual test function.
   */
  function slashCheckPrecedesSteerBranch(): void {
    const slashAt = send.indexOf("parseSlash(input)");
    const steerAt = send.indexOf("if (steerMode)");
    expect(slashAt).toBeGreaterThan(-1);
    expect(steerAt).toBeGreaterThan(-1);
    expect(slashAt).toBeLessThan(steerAt);
  }

  it("checks for slash commands BEFORE the steer branch (regression)", slashCheckPrecedesSteerBranch);

  it("keeps the slash check image-guarded (image-bearing input is a prompt/steer)", () => {
    expect(send).toContain("if (images.length === 0) {");
  });

  it("keeps question answering ahead of command routing", () => {
    // ask_user interception (freeform + numeric) must keep precedence over
    // the slash check: a pending question is being answered, and `1)`-style
    // answers never fall through to command execution.
    const numericAt = send.indexOf("if (pendingQuestion && !inFreeformMode)");
    const slashAt = send.indexOf("parseSlash(input)");
    expect(numericAt).toBeGreaterThan(-1);
    expect(slashAt).toBeGreaterThan(-1);
    expect(numericAt).toBeLessThan(slashAt);
  });
});

describe("InputBar parked banner (backlog 5c33e945: an interrupted turn must not look like a hang)", () => {
  it("renders the two manual-input reasons as a distinct banner", () => {
    // The banner is driven by `parkedNeedsInput` (interrupted |
    // budget_exhausted only — the by-design waits are evidence-only) and
    // shows the resume hint so a parked turn never looks like a hang
    // (2027-01-07 live: a mid-Executing stop looked like a hang and
    // needed a manual "c" to resume).
    expect(source).toContain("parkedNeedsInput");
    expect(source).toContain(
      'parked.reason === "interrupted" || parked.reason === "budget_exhausted"',
    );
    expect(source).toContain("Turn interrupted — send a message to resume.");
    expect(source).toContain(
      "Auto-continue budget exhausted — send a message to continue.",
    );
  });

  it("gates the banner on !running (a parked agent is idle by definition)", () => {
    expect(source).toContain("!running && parkedNeedsInput");
  });
});
