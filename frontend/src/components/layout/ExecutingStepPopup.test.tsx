// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * ExecutingStepPopup — markup + wiring tests (backlog af572504). The
 * popup must show ONLY the current step's headline as a single
 * CSS-truncated line — never the recipe body, never the other steps.
 * Markup is pinned via renderToStaticMarkup (node env, ModelCombobox
 * pattern); the StatusBar wiring is contracted against the source
 * (InflightBar ?raw pattern).
 */
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ExecutingStepPopup } from "./ExecutingStepPopup";
import statusBarSource from "./StatusBar.tsx?raw";
import type { PlanFile } from "../../lib/types";

const plan: PlanFile = {
  title: "My Plan",
  goal: "g",
  context: "c",
  steps: [
    { index: 0, text: "**Step one** — first body", header: "Step one", done: true },
    {
      index: 1,
      text: "**Step two** — the current step with a long recipe body",
      header: "Step two",
      done: false,
    },
    { index: 2, text: "**Step three** — later", header: "Step three", done: false },
  ],
};

describe("ExecutingStepPopup", () => {
  it("renders only the current step's headline — never other steps or bodies", () => {
    const html = renderToStaticMarkup(<ExecutingStepPopup plan={plan} />);
    expect(html).toContain("Step two");
    expect(html).not.toContain("Step one");
    expect(html).not.toContain("Step three");
    expect(html).not.toContain("recipe body");
    expect(html).toContain("step 2/3");
  });

  it("carries the single-line truncation", () => {
    const html = renderToStaticMarkup(<ExecutingStepPopup plan={plan} />);
    expect(html).toContain("truncate");
  });

  it("shows the first line for a headerless current step", () => {
    const plain: PlanFile = {
      ...plan,
      steps: [
        { index: 0, text: "First line of a headerless step\nSecond line", done: false },
      ],
    };
    const html = renderToStaticMarkup(<ExecutingStepPopup plan={plain} />);
    expect(html).toContain("First line of a headerless step");
    expect(html).not.toContain("Second line");
  });

  it("shows All steps complete when every step is done", () => {
    const done: PlanFile = {
      ...plan,
      steps: [{ index: 0, text: "**Only** — body", header: "Only", done: true }],
    };
    const html = renderToStaticMarkup(<ExecutingStepPopup plan={done} />);
    expect(html).toContain("All steps complete");
    expect(html).toContain("· complete");
  });
});

describe("StatusBar popup wiring (source contract)", () => {
  it("renders the ExecutingStepPopup instead of a step list", () => {
    expect(statusBarSource).toContain("<ExecutingStepPopup");
    // The old full-list dropdown is gone — no step map remains in the file.
    expect(statusBarSource).not.toContain("plan.steps.map");
  });

  it("updates the button title to the current step", () => {
    expect(statusBarSource).toContain("Click to view the current step");
  });
});
