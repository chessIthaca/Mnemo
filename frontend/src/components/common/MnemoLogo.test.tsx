// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { renderToStaticMarkup } from "react-dom/server";
import { Fragment } from "react";
import { describe, expect, it } from "vitest";
import { MnemoLogo } from "./MnemoLogo";

/** Extract every `url(#...)` reference from rendered SVG markup. */
function urlRefs(markup: string): string[] {
  return [...markup.matchAll(/url\(#([^)]+)\)/g)].map((m) => m[1]);
}

describe("MnemoLogo", () => {
  it("renders the Mnemo M-mark paths from icon-source.svg", () => {
    const markup = renderToStaticMarkup(<MnemoLogo />);
    // The distinctive "M" stroke of the app icon mark.
    expect(markup).toContain('d="M210 300L512 540L814 300"');
    // Blue active-memory node accent.
    expect(markup).toContain('#587CFF');
    // Badge background rounded rect.
    expect(markup).toContain('rx="190"');
  });

  it("namespaces def ids per instance so two instances never collide", () => {
    // Sidebar About button + About dialog header render simultaneously in the
    // real app; duplicate gradient ids would break the second instance's fills.
    const markup = renderToStaticMarkup(
      <Fragment>
        <MnemoLogo />
        <MnemoLogo className="h-6 w-6" />
      </Fragment>,
    );
    const bgRefs = urlRefs(markup).filter((id) => id.endsWith("-bg"));
    expect(bgRefs).toHaveLength(2);
    expect(new Set(bgRefs).size).toBe(2);
    // Each fill references a def id that actually exists in the markup.
    for (const id of bgRefs) expect(markup).toContain(`id="${id}"`);
  });

  it("applies the className prop to the svg element", () => {
    const markup = renderToStaticMarkup(<MnemoLogo className="h-7 w-7" />);
    expect(markup).toMatch(/<svg[^>]*class="h-7 w-7"/);
  });
});