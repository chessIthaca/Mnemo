// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, it, expect } from "vitest";
import appSource from "../App.tsx?raw";
import inflightSource from "./chat/InflightBar.tsx?raw";
import fileViewerSource from "./views/FileViewer.tsx?raw";
import graphSource from "./views/GraphView.tsx?raw";
import traceSource from "./views/LlmTraceView.tsx?raw";

/**
 * Source-contract test for the app's resize-handle motif (the repo's
 * component-test pattern: no DOM rendering — the JSX source is the
 * contract).
 *
 * Regression: user-reported "the horizontal resize bar has the wrong
 * background color — compare it to vertical ones". Every handle strip used
 * to be transparent, so the 6px seam showed whatever surface sat behind
 * it: the vertical App.tsx handle floated on the app root (bg-bg-primary,
 * #0f172a — dark) while all four horizontal handles sat on bg-bg-secondary
 * surfaces (#1e293b — InflightBar's own container, RightPanel behind the
 * views) and rendered lighter. The intended direction (user follow-up):
 * every handle PAINTS bg-bg-secondary between its 1px hairlines — the
 * lighter chrome-band token the horizontal bars always showed — vertical
 * ResizeHandle included. The seam is the token everywhere, not an accident
 * of the surrounding surface. This test fails if any handle loses that
 * background (e.g. a revert to a transparent strip, or a revert to the
 * dark bg-bg-primary direction) or drops the shared grip-pill affordance.
 */

const handles = [
  {
    name: "App.tsx ResizeHandle (vertical)",
    source: appSource,
    seam: "border-x border-border bg-bg-secondary",
  },
  {
    name: "InflightBar drag handle",
    source: inflightSource,
    seam: "border-y border-border bg-bg-secondary",
  },
  {
    name: "FileViewer tree splitter",
    source: fileViewerSource,
    seam: "border-y border-border bg-bg-secondary",
  },
  {
    name: "GraphView bottom splitter",
    source: graphSource,
    seam: "border-y border-border bg-bg-secondary",
  },
  {
    name: "LlmTraceView stats splitter",
    source: traceSource,
    seam: "border-y border-border bg-bg-secondary",
  },
];

describe("resize handle motif", () => {
  it.each(handles)(
    "$name paints the chrome-band background between its hairlines",
    ({ source, seam }) => {
      expect(source).toContain(seam);
    },
  );

  it.each(handles)("$name shows the shared grip pill", ({ source }) => {
    expect(source).toContain("bg-slate-500 group-hover:bg-slate-400");
  });
});
