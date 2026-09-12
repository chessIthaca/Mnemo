// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";
import { stepBody, stepHeadline } from "./planSteps";

/**
 * Contract for the PlanProgress step-body strip — the frontend mirror of the
 * Rust `extract_bold_header` (src/workflow/plan_file.rs). The critical
 * regression (backlog 2026-08-20): the legacy double-wrapped
 * `****Header**` steps written before create_plan started unwrapping
 * already-bold headers must strip cleanly, leaving no stray `**`.
 */
describe("stepBody", () => {
  it("strips a single-wrapped header + separator", () => {
    expect(stepBody("**Add X** — do it")).toBe("do it");
  });

  it("strips the legacy double-wrapped form without leaving stray asterisks", () => {
    // The exact legacy shape from .coding/plans/5866e160-…md.
    expect(stepBody("****Verify builds/tests** — run them")).toBe("run them");
    // Symmetric double-wrap.
    expect(stepBody("****Header**** — body")).toBe("body");
  });

  it("strips the header-only form to the empty string", () => {
    expect(stepBody("**Header**")).toBe("");
    expect(stepBody("****Header**")).toBe("");
  });

  it("passes plain text through unchanged", () => {
    expect(stepBody("Plain step with no bold")).toBe("Plain step with no bold");
  });

  it("keeps a body after other separators, or none", () => {
    expect(stepBody("**H**: body")).toBe("body");
    expect(stepBody("**H** - body")).toBe("body");
    expect(stepBody("**H** – body")).toBe("body");
    expect(stepBody("**H** body")).toBe("body");
  });

  it("matches a header containing a single * (non-greedy body)", () => {
    expect(stepBody("**src/*.rs** — clean up")).toBe("clean up");
  });

  it("does not swallow a body that merely starts with a dash", () => {
    // Only ONE separator is consumed — a body starting with `-` keeps it.
    expect(stepBody("**H** — -flag handling")).toBe("-flag handling");
  });
});

/**
 * Contract for the executing-popup headline (backlog af572504): the popup
 * shows ONLY the current step's headline — the bold header when present,
 * else the text's first non-blank line — never the recipe body.
 */
describe("stepHeadline", () => {
  it("returns the bold header when present", () => {
    expect(stepHeadline({ header: "Add X", text: "**Add X** — do it now" })).toBe("Add X");
  });

  it("falls back to the text's first non-blank line for plain steps", () => {
    expect(stepHeadline({ text: "First line\nSecond line" })).toBe("First line");
  });

  it("skips leading blank lines", () => {
    expect(stepHeadline({ text: "\n\n  \nFirst line\nSecond" })).toBe("First line");
  });

  it("falls back to the raw text when every line is blank", () => {
    expect(stepHeadline({ text: " \n\t\n" })).toBe(" \n\t\n");
  });

  it("treats a null header as absent", () => {
    expect(stepHeadline({ header: null, text: "Only line" })).toBe("Only line");
  });
});
