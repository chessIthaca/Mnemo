// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * StatusBar bottom-bar model/provider truncation contract test.
 *
 * The model and provider name spans in the StatusBar were unbounded: under a
 * narrow window, flex squeezed them and the text wrapped at hyphens/spaces
 * ("glm-" / "5.3-flash" / "Ollama" / "Cloud"), growing the bar to multiple
 * lines. Like the other fixed-size badges in the same bar (Complete / max /
 * main), both spans must clamp to a fixed max width and cut off with an
 * ellipsis (`truncate` = overflow-hidden + text-ellipsis + whitespace-nowrap)
 * so the bar always stays single-line.
 *
 * This project has no React DOM test infra (vitest runs in `node`
 * environment), so this is a static source-contract test in the style of
 * `MainPanel.tabStyle.test.ts`: it reads the component source via Vite's
 * `?raw` import (typed by `vite/client`, so it compiles under `tsc` too).
 */

import { describe, expect, it } from "vitest";
import statusBarSource from "./StatusBar.tsx?raw";

describe("StatusBar model/provider fixed-width truncation (single-line bar)", () => {
  it("model span clamps to a fixed width and ellipsizes", () => {
    expect(statusBarSource).toContain(
      "max-w-44 truncate font-medium text-cyan-400",
    );
  });

  it("provider span clamps to a fixed width, ellipsizes, and exposes the full name on hover", () => {
    expect(statusBarSource).toContain("max-w-32 truncate text-blue-400");
    expect(statusBarSource).toContain("title={activeEndpoint}");
  });

  // Review follow-up (report 2026-08-31-statusbar-truncate-review.md, F1+F2):
  // the same multiline vector existed in three other bar spans, and the
  // truncated model name had no hover fallback. Pin those fixes too.
  it("model span tooltip leads with the full model name so truncation stays discoverable", () => {
    expect(statusBarSource).toContain(
      "${effectiveModel} — The active agent's effective model",
    );
  });

  it("workflow state label clamps and reveals the full label on hover", () => {
    expect(statusBarSource).toContain("max-w-24 truncate text-green-400");
    expect(statusBarSource).toContain("title={stateLabel}");
  });

  it("git branch span clamps and reveals the full branch name on hover", () => {
    expect(statusBarSource).toContain("max-w-32 truncate text-purple-400");
    expect(statusBarSource).toContain("title={gitBranch}");
  });

  it("merge-to-main button label never wraps", () => {
    expect(statusBarSource).toContain(
      "flex items-center gap-1 rounded px-2 py-0.5 text-xs font-medium text-cyan-400 transition-colors hover:bg-bg-tertiary whitespace-nowrap",
    );
  });

  // Review follow-up pass 2 (report 2026-08-31-statusbar-truncate-review-pass2.md):
  // the safety-mode button label ("Approve Each" / "Auto Project") is the last
  // spaced-text item in the bar without the nowrap treatment — same class of
  // multiline-squeeze defect.
  it("safety-mode button label never wraps", () => {
    expect(statusBarSource).toContain(
      "flex items-center gap-1.5 whitespace-nowrap rounded px-2 py-0.5 text-xs font-medium transition-colors",
    );
  });
});