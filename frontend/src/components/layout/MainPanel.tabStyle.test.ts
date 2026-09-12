// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Agent tab-bar style contract test.
 *
 * The MainPanel agent tabs ("model tabs") were restyled to the same flat
 * rail as the RightPanel tool tabs: fixed h-10 triggers with a cyan 2px
 * bottom border on the active tab — no raised pill (rounded-t-md + active
 * bg). This file locks that contract so the two tab bars cannot drift apart
 * again (previously the agent tab was a raised pill while the tool tabs were
 * a flat rail, and they read as misaligned heights).
 *
 * This project has no React DOM test infra (vitest runs in `node`
 * environment), so this is a static source-contract test in the style of
 * `src/components/chat/InflightBar.test.ts`: it reads the component source
 * (via Vite's `?raw` import — typed by `vite/client`, so it compiles under
 * `tsc` too) and asserts the styling contract is present.
 */

import { describe, expect, it } from "vitest";
import mainPanelSource from "./MainPanel.tsx?raw";
import rightPanelSource from "./RightPanel.tsx?raw";

describe("MainPanel agent tab bar rail style (parity with RightPanel tool tabs)", () => {
  it("active tab carries the cyan bottom-border rail (border-cyan-500 text-cyan-400)", () => {
    // The active branch of the trigger className template — the same motif
    // the RightPanel tool tabs use for the active tab.
    expect(mainPanelSource).toContain(
      '? "border-cyan-500 text-cyan-400"',
    );
  });

  it("inactive tab carries a transparent border (no raised look)", () => {
    expect(mainPanelSource).toContain(
      ': "border-transparent text-slate-500 hover:text-slate-300"',
    );
  });

  it("trigger is a fixed h-10 rail item (no pill padding)", () => {
    // Height parity with the RightPanel h-10 triggers. The pill's
    // py-1.5/rounded-t-lg raised look must be gone.
    expect(mainPanelSource).toMatch(/flex h-10 items-center gap-2 border-b-2/);
    expect(mainPanelSource).not.toContain("rounded-t-md");
  });

  it("keeps the agent-name font size text-sm (two-line tab: name + model)", () => {
    // The rail alignment drives height; the name must stay legible above
    // the model second line.
    expect(mainPanelSource).toContain(
      "flex h-10 items-center gap-2 border-b-2 px-3 text-sm",
    );
  });

  it("keeps the rail on its own bg-bg-secondary chrome band", () => {
    // The tab bar must read as a distinct surface above the bg-bg-primary
    // conversation (matching Sidebar/RightPanel/InflightBar/StatusBar/
    // InputBar). A future restyle that drops the background again would
    // make the rail blend into the conversation.
    expect(mainPanelSource).toContain(
      "flex items-stretch overflow-x-auto border-b border-border bg-bg-secondary",
    );
  });

  it("preserves the model second line, running dot, and subagent close-x", () => {
    // Regression guard: the restyle must not drop the model display or the
    // close affordance (both are what makes this a *model* tab).
    expect(mainPanelSource).toContain("truncate text-[0.625em] text-slate-500");
    expect(mainPanelSource).toContain("agent-running-dot");
    expect(mainPanelSource).toContain("Close subagent");
  });
});

describe("RightPanel tool-tab rail (reference, must not drift)", () => {
  it("keeps h-10 triggers with the cyan border-b-2 rail", () => {
    // The rail the agent tabs now mirror. If the tool rail ever changes
    // style, this test flags it so the parity decision is revisited.
    expect(rightPanelSource).toContain(
      "flex h-10 items-center gap-1.5 border-b-2",
    );
    expect(rightPanelSource).toContain(
      '? "border-cyan-500 text-cyan-400"',
    );
  });
});
