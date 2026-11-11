// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";
import { parseNumericAnswer } from "./answerSelect";

describe("parseNumericAnswer", () => {
  const optionCount = 3; // options: 1,2,3 ; freeform = 4

  it("maps 1..optionCount to a choice index", () => {
    expect(parseNumericAnswer("1)", optionCount)).toEqual({ kind: "choice", index: 0 });
    expect(parseNumericAnswer("2)", optionCount)).toEqual({ kind: "choice", index: 1 });
    expect(parseNumericAnswer("3)", optionCount)).toEqual({ kind: "choice", index: 2 });
  });

  it("maps optionCount+1 to freeform (the 'Let's talk about it' choice)", () => {
    expect(parseNumericAnswer("4)", optionCount)).toEqual({ kind: "freeform" });
  });

  it("trims surrounding whitespace before matching", () => {
    expect(parseNumericAnswer("  2)  ", optionCount)).toEqual({ kind: "choice", index: 1 });
  });

  it("returns null for out-of-range numbers", () => {
    expect(parseNumericAnswer("5)", optionCount)).toBeNull();
    expect(parseNumericAnswer("0)", optionCount)).toBeNull();
    expect(parseNumericAnswer("99)", optionCount)).toBeNull();
  });

  it("returns null when the input is not the N) syntax", () => {
    // A bare number (no `)`) must NOT match — it would hijack ordinary numeric
    // prompts the user is typing.
    expect(parseNumericAnswer("2", optionCount)).toBeNull();
    expect(parseNumericAnswer("hello", optionCount)).toBeNull();
    expect(parseNumericAnswer("", optionCount)).toBeNull();
    expect(parseNumericAnswer("1.", optionCount)).toBeNull();
    expect(parseNumericAnswer("1)", 0)).toEqual({ kind: "freeform" }); // no options → 1) is freeform
  });

  it("handles multi-digit numbers", () => {
    expect(parseNumericAnswer("12)", 12)).toEqual({ kind: "choice", index: 11 });
    expect(parseNumericAnswer("13)", 12)).toEqual({ kind: "freeform" });
    expect(parseNumericAnswer("14)", 12)).toBeNull();
  });
});
