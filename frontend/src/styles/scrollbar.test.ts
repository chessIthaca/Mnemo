// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Regression suite for the scrollbar thumb filling its whole track: the thumb
 * must be inset by 1px on all four sides so a thin track-colored gap shows
 * between the thumb and its container (including on hover). The inset is done
 * with a transparent 1px border + `background-clip: padding-box`; because the
 * `background` shorthand resets background-clip to border-box, both thumb
 * rules (base and :hover) must use the background-color longhand or the gap
 * silently disappears. This is a CSS-contract test: it fails if anyone
 * "simplifies" the rules back to the shorthand or drops the border/clip.
 *
 * (Static source-contract style like InflightBar.test.ts, but the stylesheet
 * is read from disk via node:fs instead of Vite's `?raw` import: vitest runs
 * with CSS processing disabled (`css: false` default), which stubs every
 * `.css` import — including `?raw` and `?inline` — to an empty string, so the
 * rules can't be seen that way. The project has no @types/node; the minimal
 * ambient `node:fs` declaration lives in src/node-shims.d.ts.)
 */
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./globals.css", import.meta.url), "utf8");

/** Return the declaration body of the ::-webkit-scrollbar-thumb rule. */
function thumbRuleBody(hover: boolean): string {
  const re = /::-webkit-scrollbar-thumb(:hover)?\s*\{([^}]*)\}/g;
  for (const m of css.matchAll(re)) {
    if ((m[1] === ":hover") === hover) return m[2];
  }
  throw new Error(`::-webkit-scrollbar-thumb${hover ? ":hover" : ""} rule not found in globals.css`);
}

/** Declarations with comments stripped and whitespace collapsed. */
function normalized(body: string): string {
  return body.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\s+/g, " ");
}

describe("scrollbar thumb inset (globals.css)", () => {
  it("base thumb rule insets the thumb 1px on all four sides", () => {
    const body = normalized(thumbRuleBody(false));
    // Whitespace-tolerant regexes: only the declarations' presence matters, so
    // a valid reformat (e.g. `border:1px solid transparent`) can't false-fail.
    expect(body).toMatch(/border\s*:\s*1px\s+solid\s+transparent/);
    expect(body).toMatch(/background-clip\s*:\s*padding-box/);
  });

  it("base + hover thumb rules use background-color, never the background shorthand", () => {
    // The `background` shorthand resets background-clip to border-box, which
    // would erase the 1px inset gap — on hover too, because the :hover rule's
    // shorthand would override the base rule's clip at higher specificity.
    for (const hover of [false, true]) {
      const body = normalized(thumbRuleBody(hover));
      expect(body).toMatch(/background-color\s*:/);
      expect(body).not.toMatch(/(^|;)\s*background\s*:/);
    }
  });
});

describe("scrollbar corner (globals.css)", () => {
  /** Return the declaration body of the ::-webkit-scrollbar-corner rule. */
  function cornerRuleBody(): string {
    const m = css.match(/::-webkit-scrollbar-corner\s*\{([^}]*)\}/);
    if (!m) {
      throw new Error("::-webkit-scrollbar-corner rule not found in globals.css");
    }
    return m[1];
  }

  it("corner is painted the track color (never Chromium's white default)", () => {
    // Regression (user-reported "white little square"): Chromium's default
    // scrollbar corner is white and blazed whenever BOTH scrollbars were
    // visible. The corner must carry an explicit background so the white
    // default can never leak through again.
    const body = normalized(cornerRuleBody());
    expect(body).toMatch(/background-color\s*:\s*var\(--bg-secondary\)/);
  });

  it("corner rule uses background-color, never the background shorthand", () => {
    // Same contract as the thumb rules: the shorthand resets background-clip
    // and reads as a "simple" background — a future refactor could
    // "simplify" this rule and silently regress the fix.
    const body = normalized(cornerRuleBody());
    expect(body).toMatch(/background-color\s*:/);
    expect(body).not.toMatch(/(^|;)\s*background\s*:/);
  });
});
