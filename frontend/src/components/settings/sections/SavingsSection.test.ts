// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Savings section — two halves, one section.
 *
 * Model rates (backlog 82dd66fc, user-reported: the model field was type-only;
 * a typo'd row silently never applies because pricing is keyed by exact model
 * name — Config::pricing_for, consumed by StatsView's modelCost). The four
 * required behaviors: the dropdown lists the configured models; selecting an
 * entry sets the field value; free-typed values are preserved on save; a typed
 * model not in the list still saves.
 *
 * Token-optimizer levers (backlog ebe21e3c, [general.optimizer]): the six flags
 * + four knobs + the extra-command list ride the SAME save patch as the pricing
 * rows, and both halves count towards the section's dirty state.
 *
 * Node environment — static source contracts in the ChatSection/InflightBar
 * style; the data layer (grouping/dedup/annotation/filter) is unit-tested in
 * modelOptions.test.ts and the combobox's own markup/contracts in
 * ModelCombobox.test.tsx.
 */

import { describe, expect, it } from "vitest";
import source from "./SavingsSection.tsx?raw";

describe("Savings model combobox wiring", () => {
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
    // handleSave's pricing half is unchanged: trim, numeric coercion, filter
    // non-empty, and the cleaned rows ride the save patch (alongside the
    // optimizer fields) — the field's value saves as typed.
    expect(source).toContain("model: r.model.trim()");
    expect(source).toContain(".filter((r) => r.model)");
    expect(source).toContain("saveSettings({");
    expect(source).toContain("pricing: cleaned,");
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

describe("Optimizer levers in the Savings section", () => {
  const FLAGS = [
    "delta_reads",
    "compress_output",
    "archive",
    "compaction_survival",
    "quality_score",
    "lean_output_nudge",
  ];
  const KNOBS = [
    "archive_min_chars",
    "compress_min_chars",
    "lean_output_fill_pct",
    "nudge_cooldown_requests",
  ];

  it("renders all six levers and all four knobs", () => {
    // BOOL_FIELDS drives the toggle list and KNOBS the number inputs — a lever
    // or knob added backend-side without this file listing it fails loudly
    // here instead of going silently missing from the UI.
    for (const f of FLAGS) expect(source).toContain(`"${f}"`);
    for (const k of KNOBS) expect(source).toContain(`"${k}"`);
  });

  it("reads the levers off the get_settings optimizer block", () => {
    expect(source).toContain("const o = s.general.optimizer;");
    expect(source).toContain("for (const k of BOOL_FIELDS) f[k] = o[k];");
    expect(source).toContain("setFlags(f);");
    expect(source).toContain("setKnobs(k);");
  });

  it("counts both halves towards the dirty state", () => {
    // One snapshot covers rates + flags + knobs + the extra-command text, so
    // editing a lever dirties the section exactly like editing a rate does —
    // the dialog shell then saves it on OK.
    expect(source).toContain(
      "JSON.stringify({ pricing: rows, flags, knobs, extra: extraCommands }) !== snapshot",
    );
    expect(source).toContain(
      "setSnapshot(JSON.stringify({ pricing: p, flags: f, knobs: k, extra }))",
    );
  });

  it("sends every lever and knob in the same save patch as the rates", () => {
    expect(source).toContain("optimizer_delta_reads: flags.delta_reads,");
    expect(source).toContain("optimizer_archive: flags.archive,");
    expect(source).toContain("optimizer_lean_output_nudge: flags.lean_output_nudge,");
    expect(source).toContain("optimizer_archive_min_chars: knobs.archive_min_chars,");
    expect(source).toContain(
      "optimizer_nudge_cooldown_requests: knobs.nudge_cooldown_requests,",
    );
    // The rates ride the same call: one patch, one config rewrite.
    expect(source).toContain("pricing: cleaned,");
  });

  it("edits the extra-command list as newline-separated text", () => {
    // Loaded joined into the textarea, saved split/trimmed/blanks-dropped —
    // matching the backend, which trims and drops blanks again.
    expect(source).toContain("(o.compress_extra_commands ?? []).join(");
    expect(source).toContain(".map((c) => c.trim())");
    expect(source).toContain(".filter((c) => c);");
    expect(source).toContain("optimizer_compress_extra_commands: extra,");
  });

  it("only renders the controls once the optimizer block has loaded", () => {
    // A null draft (pre-load) renders nothing rather than an all-off editor
    // that could save over the user's real config.
    expect(source).toContain("{flags && knobs && (");
    expect(source).toContain("if (!flags || !knobs) return false;");
  });
});
