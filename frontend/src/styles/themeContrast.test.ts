// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * WCAG contrast regression guard for the two theme token blocks in
 * globals.css (`:root` dark, `html.light` light). The light theme shipped
 * unreadable (user report 2026-08-22: "the others are terrible and
 * unreadable") — the shared cyan-400 accent was ≈1.8:1 on white because
 * `html.light` never overrode `--accent-color`. This test reads the CSS
 * source from disk so the ratios are asserted on the shipped tokens, and
 * FAILS LOUDLY on a missing/renamed token (never silently passes on a stale
 * CSS shape).
 */

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

// Read the stylesheet from disk: vitest runs with CSS processing disabled
// (`css: false` default), which stubs every `.css` import — including `?raw`
// — to an empty string (same pattern as scrollbar.test.ts; the ambient
// `node:fs` declaration lives in src/node-shims.d.ts).
const globalsCss = readFileSync(new URL("./globals.css", import.meta.url), "utf8");

/** Extract `--token: #hex` declarations from one CSS block body. */
function parseTokens(blockBody: string): Map<string, string> {
  const out = new Map<string, string>();
  for (const m of blockBody.matchAll(/--([\w-]+):\s*(#[0-9a-fA-F]{6})\s*;/g)) {
    out.set(m[1], m[2].toLowerCase());
  }
  return out;
}

/** Extract the body of a `selector { ... }` block (single-brace blocks). */
function blockBody(css: string, selector: string): string {
  const start = css.indexOf(selector);
  if (start === -1) throw new Error(`selector ${selector} not found in globals.css`);
  const open = css.indexOf("{", start + selector.length);
  const close = css.indexOf("}", open);
  if (open === -1 || close === -1) {
    throw new Error(`selector ${selector} has no brace block in globals.css`);
  }
  return css.slice(open + 1, close);
}

/** WCAG 2.x relative luminance of a #rrggbb color. */
function luminance(hex: string): number {
  const chan = (i: number) => {
    const c = parseInt(hex.slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * chan(1) + 0.7152 * chan(3) + 0.0722 * chan(5);
}

/** WCAG contrast ratio between two #rrggbb colors (1–21). */
function contrast(a: string, b: string): number {
  const la = luminance(a);
  const lb = luminance(b);
  const [hi, lo] = la >= lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

const dark = parseTokens(blockBody(globalsCss, ":root"));
const light = parseTokens(blockBody(globalsCss, "html.light"));

/** Read a token or fail the test with a named, actionable error. */
function tok(theme: Map<string, string>, name: string, themeName: string): string {
  const v = theme.get(name);
  if (!v) throw new Error(`--${name} missing from the ${themeName} block in globals.css`);
  return v;
}

const THEMES: Array<{ name: string; tokens: Map<string, string> }> = [
  { name: "dark", tokens: dark },
  { name: "light", tokens: light },
];

describe("theme token contrast (WCAG)", () => {
  for (const { name, tokens } of THEMES) {
    it(`${name}: primary text ≥ 7:1 on the primary background`, () => {
      const ratio = contrast(
        tok(tokens, "text-primary", name),
        tok(tokens, "bg-primary", name),
      );
      expect(ratio).toBeGreaterThanOrEqual(7);
    });

    it(`${name}: muted text ≥ 4.5:1 on the secondary background`, () => {
      const ratio = contrast(
        tok(tokens, "text-muted", name),
        tok(tokens, "bg-secondary", name),
      );
      expect(ratio).toBeGreaterThanOrEqual(4.5);
    });

    it(`${name}: accent ≥ 4.5:1 on the primary background (accent text/links)`, () => {
      const ratio = contrast(
        tok(tokens, "accent-color", name),
        tok(tokens, "bg-primary", name),
      );
      expect(ratio).toBeGreaterThanOrEqual(4.5);
    });

    it(`${name}: accent-contrast text ≥ 4.5:1 on accent-filled buttons`, () => {
      const ratio = contrast(
        tok(tokens, "accent-contrast-text", name),
        tok(tokens, "accent-color", name),
      );
      expect(ratio).toBeGreaterThanOrEqual(4.5);
    });
  }

  it("light theme overrides --accent-color (the original unreadable-accent bug)", () => {
    // Regression: html.light once inherited the cyan-400 dark accent
    // (≈1.9:1 on white). The light block must declare its own darker accent.
    expect(light.has("accent-color")).toBe(true);
    expect(tok(light, "accent-color", "light")).not.toBe(
      tok(dark, "accent-color", "dark"),
    );
  });
});
