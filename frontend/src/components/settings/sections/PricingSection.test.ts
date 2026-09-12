// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Pricing section — model combobox wiring tests (backlog 82dd66fc,
 * user-reported: the model field was type-only; a typo'd row silently never
 * applies because pricing is keyed by exact model name — Config::pricing_for,
 * consumed by StatsView's modelCost). Node environment — static source
 * contracts in the ChatSection/InflightBar style; the data layer
 * (grouping/dedup/annotation/filter) is unit-tested in modelOptions.test.ts
 * and the combobox's own markup/contracts in ModelCombobox.test.tsx. These
 * pin the four required behaviors: the dropdown lists the configured
 * models; selecting an entry sets the field value; free-typed values are
 * preserved on save; a typed model not in the list still saves.
 */

import { describe, expect, it } from "vitest";
import source from "./PricingSection.tsx?raw";

describe("Pricing model combobox wiring", () => {
  it("the model field is the combobox, controlled by the row's model value", () => {
    expect(source).toContain("<ModelCombobox");
    expect(source).toMatch(/value=\{r\.model\}/);
    // Selecting a dropdown entry (and free typing — both flow through the
    // combobox's onChange) updates the row via the same updateRow path the
    // old input used.
    expect(source).toContain("onChange={(v) => updateRow(i, { model: v })}");
  });

  it("the dropdown lists the configured endpoints' models", () => {
    // Options are built from getSettings().endpoints — the models lists
    // the configured endpoints declare (grouped/deduped/annotated by
    // buildModelOptions; see modelOptions.test.ts).
    expect(source).toContain("buildModelOptions(s.endpoints)");
    expect(source).toContain("options={modelOptions}");
  });

  it("free-typed values are preserved on save — trim + non-empty only", () => {
    // handleSave is unchanged: trim, numeric coercion, filter non-empty,
    // saveSettings({ pricing }) — the field's value saves as typed.
    expect(source).toContain("model: r.model.trim()");
    expect(source).toContain(".filter((r) => r.model)");
    expect(source).toContain("saveSettings({ pricing: cleaned })");
  });

  it("a typed model not in the list still saves — no membership validation", () => {
    // The save path never checks the options list: no includes/some gate
    // against modelOptions, no filterModelOptions call in the section —
    // free text for models the configured lists don't carry still saves.
    expect(source).not.toContain("modelOptions.includes");
    expect(source).not.toContain("options.some");
    expect(source).not.toContain("filterModelOptions");
  });
});
