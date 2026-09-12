// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect } from "vitest";
import { numericKnobDirty } from "./AdvancedSection";

/**
 * Regression tests for the numeric-knob dirty predicate (reviews I4 + F3,
 * 2026-08-18): a cleared field (`Number("") === 0`) or stray text (`NaN`)
 * cannot be saved, so it must not count as dirty — otherwise the section
 * latches dirty forever (`NaN !== NaN`) or a cleared field "saves" nothing
 * while clearing the dirty flag. Valid-but-unchanged also stays clean.
 */

describe("numericKnobDirty (I4 + F3 regression)", () => {
  it("clean when equal to the snapshot (integer knob, no eps)", () => {
    expect(numericKnobDirty(16, 16, 1)).toBe(false);
    expect(numericKnobDirty(256, 256, 1)).toBe(false);
  });

  it("dirty when a valid value differs from the snapshot", () => {
    expect(numericKnobDirty(64, 16, 1)).toBe(true);
    expect(numericKnobDirty(16, 64, 1)).toBe(true);
  });

  it("clean when NaN — NaN !== NaN must not latch dirty", () => {
    expect(numericKnobDirty(Number.NaN, 16, 1)).toBe(false);
  });

  it("clean when below the knob's minimum (cleared field: Number('') === 0)", () => {
    // The pre-I4 bug: 0 !== 16 latched the section dirty and, on save,
    // the snap was overwritten with the unsavable value.
    expect(numericKnobDirty(0, 16, 1)).toBe(false);
  });

  it("clean when equal to the minimum (a valid value)", () => {
    expect(numericKnobDirty(1, 1, 1)).toBe(false);
    expect(numericKnobDirty(1, 16, 1)).toBe(true);
  });

  it("float knob: dirty only past the epsilon", () => {
    // The fill-rate slider (min 0.05, eps 1e-9): sub-epsilon drift is
    // slider noise, not a change.
    expect(numericKnobDirty(0.5, 0.5 + 1e-12, 0.05, 1e-9)).toBe(false);
    expect(numericKnobDirty(0.5, 0.55, 0.05, 1e-9)).toBe(true);
  });

  it("float knob: NaN and below-min stay clean too", () => {
    expect(numericKnobDirty(Number.NaN, 0.5, 0.05, 1e-9)).toBe(false);
    expect(numericKnobDirty(0, 0.5, 0.05, 1e-9)).toBe(false);
  });
});
