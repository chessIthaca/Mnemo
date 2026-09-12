// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * ModelCombobox — markup + source-contract tests (backlog 82dd66fc). The
 * vitest suite runs in a node environment (no React DOM test infra), so the
 * closed-state markup is pinned via renderToStaticMarkup (the
 * MnemoLogo/SplashCard pattern) and the interactive wiring — dropdown
 * entries call onChange with the model id, free text passes through,
 * Escape/overlay close — via static source contracts (the
 * InflightBar/ChatSection pattern).
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ModelCombobox } from "./ModelCombobox";
import source from "./ModelCombobox.tsx?raw";

const options = [
  { model: "glm-5.3-gcp", endpoints: ["openrouter", "lmstudio"] },
  { model: "deepseek-v4-flash", endpoints: ["alpha"] },
];

describe("ModelCombobox markup (closed state)", () => {
  it("renders a combobox input carrying the current value", () => {
    const markup = renderToStaticMarkup(
      <ModelCombobox value="glm-5.3-gcp" onChange={() => {}} options={options} />,
    );
    expect(markup).toContain('role="combobox"');
    expect(markup).toContain('value="glm-5.3-gcp"');
    expect(markup).toContain('aria-expanded="false"');
  });

  it("renders the toggle button and no dropdown entries while closed", () => {
    const markup = renderToStaticMarkup(
      <ModelCombobox value="" onChange={() => {}} options={options} />,
    );
    expect(markup).toContain("Toggle model list");
    // The option list only renders when open — the closed field must not
    // leak entries into the row layout.
    expect(markup).not.toContain("glm-5.3-gcp");
    expect(markup).not.toContain("deepseek-v4-flash");
  });

  it("carries the ARIA 1.2 combobox attributes (review round 1, L1)", () => {
    const markup = renderToStaticMarkup(
      <ModelCombobox value="glm-5.3-gcp" onChange={() => {}} options={options} />,
    );
    expect(markup).toContain('aria-autocomplete="list"');
  });
});

describe("ModelCombobox source contracts (interactive wiring)", () => {
  it("free text passes through: the input's onChange forwards e.target.value", () => {
    expect(source).toContain("onChange={(e) => onChange(e.target.value)}");
  });

  it("selecting an entry fills the field with the model id and closes", () => {
    expect(source).toContain("onChange(o.model);");
    expect(source).toMatch(/onClick=\{\(\) => \{\s*onChange\(o\.model\);\s*setOpen\(false\);/);
  });

  it("typing filters the list via the shared pure rule", () => {
    expect(source).toContain("filterModelOptions(options, value)");
  });

  it("Escape and click-outside close the dropdown", () => {
    expect(source).toContain('e.key === "Escape"');
    expect(source).toContain("onClick={() => setOpen(false)}");
  });
});

describe("ModelCombobox source contracts (review round 1 — a11y + focus)", () => {
  it("the popup is a listbox with option entries (L1)", () => {
    expect(source).toContain('role="listbox"');
    expect(source).toContain('role="option"');
    expect(source).toContain('aria-autocomplete="list"');
  });

  it("selecting returns focus to the field without reopening the list (L2)", () => {
    expect(source).toContain("skipOpenRef.current = true;");
    expect(source).toContain("inputRef.current?.focus();");
    // The guard is consumed by onFocus so the programmatic refocus keeps
    // the dropdown closed.
    expect(source).toMatch(/if \(skipOpenRef\.current\) \{\s*skipOpenRef\.current = false;/);
  });

  it("the dropdown closes when focus leaves the combobox root (L3)", () => {
    expect(source).toContain("rootRef.current?.contains(to)");
    expect(source).toContain("onBlur={onBlur}");
  });
});

describe("ModelCombobox source contracts (review round 2 — cross-platform + listbox structure)", () => {
  it("entry and toggle buttons prevent the mousedown focus change (WebKit/macOS: the root-blur close must not unmount the dropdown before the click fires)", () => {
    // Exactly two guards: one on the toggle, one on every entry button.
    expect(
      source.match(/onMouseDown=\{\(e\) => e\.preventDefault\(\)\}/g)?.length,
    ).toBe(2);
  });

  it("the refocus only engages when focus actually moved — no stale skipOpenRef flag", () => {
    expect(source).toContain("document.activeElement !== inputRef.current");
  });

  it("the empty-filter message renders outside the listbox (ARIA required-owned-elements)", () => {
    // aria-controls points at the listbox only when options exist; the
    // message div is a sibling of the listbox, not a child of it.
    expect(source).toContain("visible.length > 0 ? listId : undefined");
  });

  it("toggling from outside the root pulls focus into the field — the blur-close stays armed (round 3 F3)", () => {
    // The bridge between the toggle's setOpen and its guard tolerates only
    // whitespace and // comment lines — it cannot jump to the entry
    // button's guard further down, so a removed toggle guard fails the pin.
    expect(source).toMatch(
      /onClick=\{\(\) => \{\s*setOpen\(\(o\) => !o\);(?:\s|\/\/[^\n]*)*if \(document\.activeElement !== inputRef\.current\) \{\s*skipOpenRef\.current = true;\s*inputRef\.current\?\.focus\(\);/,
    );
  });
});
