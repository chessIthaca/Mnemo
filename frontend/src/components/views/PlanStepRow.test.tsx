// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * PlanStepRow — markup tests (backlog af572504). The vitest suite runs
 * in a node environment (no React DOM test infra), so the collapsed and
 * expanded markup is pinned via renderToStaticMarkup (the
 * MnemoLogo/SplashCard/ModelCombobox pattern). The interactive wiring —
 * auto-expand of the active step, override toggles — is contracted in
 * PlanProgress.expand.test.ts.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { PlanStepRow } from "./PlanStepRow";
import type { PlanStep } from "../../lib/types";

const headered: PlanStep = {
  index: 0,
  text: "**Add X** — do it now",
  header: "Add X",
  done: false,
};

describe("PlanStepRow", () => {
  it("renders the headline with a collapsed chevron and hides the details", () => {
    const html = renderToStaticMarkup(
      <PlanStepRow step={headered} expanded={false} onToggle={() => {}} />,
    );
    expect(html).toContain("Add X");
    expect(html).toContain('aria-expanded="false"');
    // Collapsed: the chevron is not rotated and the body is absent.
    expect(html).not.toContain("rotate-90");
    expect(html).not.toContain("do it now");
  });

  it("reveals the details with a rotated chevron when expanded", () => {
    const html = renderToStaticMarkup(
      <PlanStepRow step={headered} expanded={true} onToggle={() => {}} />,
    );
    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain("rotate-90");
    expect(html).toContain("do it now");
  });

  it("renders plain headerless steps as full text with no toggle", () => {
    // stepBody passes plain text through unchanged — there is nothing to
    // reveal, so no chevron/button at all.
    const plain: PlanStep = { index: 1, text: "Plain step text", done: false };
    const html = renderToStaticMarkup(
      <PlanStepRow step={plain} expanded={false} onToggle={() => {}} />,
    );
    expect(html).toContain("Plain step text");
    expect(html).not.toContain("aria-expanded");
    expect(html).not.toContain("<button");
  });

  it("renders header-only steps (empty body) without a chevron", () => {
    const headerOnly: PlanStep = {
      index: 2,
      text: "**Only header**",
      header: "Only header",
      done: false,
    };
    const html = renderToStaticMarkup(
      <PlanStepRow step={headerOnly} expanded={false} onToggle={() => {}} />,
    );
    expect(html).toContain("Only header");
    expect(html).not.toContain("aria-expanded");
    expect(html).not.toContain("<button");
  });

  it("keeps the done-step styling (line-through over header and body)", () => {
    const done: PlanStep = {
      index: 3,
      text: "**Done thing** — finished",
      header: "Done thing",
      done: true,
    };
    const html = renderToStaticMarkup(
      <PlanStepRow step={done} expanded={true} onToggle={() => {}} />,
    );
    expect(html).toContain("line-through");
    expect(html).toContain("finished");
  });
});
