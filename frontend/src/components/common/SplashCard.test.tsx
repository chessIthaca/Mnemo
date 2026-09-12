// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { renderToStaticMarkup } from "react-dom/server";
import { Fragment } from "react";
import { describe, expect, it } from "vitest";
import { SplashCard, SplashProgress } from "./SplashCard";

/** Extract every `url(#...)` reference from rendered SVG markup. */
function urlRefs(markup: string): string[] {
  return [...markup.matchAll(/url\(#([^)]+)\)/g)].map((m) => m[1]);
}

describe("SplashCard", () => {
  it("renders the Mnemo brand stack: wordmark, tagline, values", () => {
    const markup = renderToStaticMarkup(
      <SplashCard>
        <div>content</div>
      </SplashCard>,
    );
    expect(markup).toContain("mnemo");
    expect(markup).toContain("SMART CODING HARNESS");
    expect(markup).toContain("MEMORY. SPEC. IMPROVEMENT.");
  });

  it("renders the network motif in the icon-source palette", () => {
    const markup = renderToStaticMarkup(
      <SplashCard>
        <div />
      </SplashCard>,
    );
    // Brand blue glow + network edge/node tones from the promo artwork.
    expect(markup).toContain("#587CFF");
    expect(markup).toContain("#6D8BFF");
    expect(markup).toContain("#2E4A8F");
    expect(markup).toContain("#4C7DFF");
  });

  it("namespaces def ids per instance so two cards never collide", () => {
    // IndexingOverlay + the reconcile dialog can be visible simultaneously in
    // App's tree; duplicate gradient ids would break the second card's fill.
    const markup = renderToStaticMarkup(
      <Fragment>
        <SplashCard>
          <div>a</div>
        </SplashCard>
        <SplashCard>
          <div>b</div>
        </SplashCard>
      </Fragment>,
    );
    const glowRefs = urlRefs(markup).filter((id) => id.endsWith("-splashglow"));
    expect(glowRefs).toHaveLength(2);
    expect(new Set(glowRefs).size).toBe(2);
    // Each fill references a def id that actually exists in the markup.
    for (const id of glowRefs) expect(markup).toContain(`id="${id}"`);
  });

  it("renders the children content slot below the brand band", () => {
    const markup = renderToStaticMarkup(
      <SplashCard>
        <div>UNIQUE_CONTENT_MARKER</div>
      </SplashCard>,
    );
    expect(markup).toContain("UNIQUE_CONTENT_MARKER");
    // Content comes after the wordmark in document order.
    expect(markup.indexOf("UNIQUE_CONTENT_MARKER")).toBeGreaterThan(
      markup.indexOf("mnemo"),
    );
  });

  it("passes the className prop through to the card root", () => {
    const markup = renderToStaticMarkup(
      <SplashCard className="w-80">
        <div />
      </SplashCard>,
    );
    expect(markup).toContain("w-80");
  });
});

describe("SplashProgress", () => {
  it("renders the label and mono counter", () => {
    const markup = renderToStaticMarkup(
      <SplashProgress label="Indexing files…" right="12/240 files indexed" pct={5} />,
    );
    expect(markup).toContain("Indexing files…");
    expect(markup).toContain("12/240 files indexed");
  });

  it("renders the known-total fill width", () => {
    const markup = renderToStaticMarkup(
      <SplashProgress label="l" right="r" pct={40} />,
    );
    expect(markup).toContain("width:40%");
  });

  it("renders the indeterminate 30% stub when pct is null", () => {
    const markup = renderToStaticMarkup(
      <SplashProgress label="l" right="…" pct={null} />,
    );
    expect(markup).toContain("width:30%");
  });
});
