// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import appSource from "../../App.tsx?raw";
import { BootSplash } from "./BootSplash";

describe("BootSplash", () => {
  it("renders the wordmark and the starting label — the pre-index boot feedback", () => {
    const markup = renderToStaticMarkup(<BootSplash />);
    expect(markup).toContain("mnemo");
    expect(markup).toContain("Starting…");
  });

  it("is the full-screen centered splash (no empty main-UI shell flash)", () => {
    const markup = renderToStaticMarkup(<BootSplash />);
    expect(markup).toContain("h-screen w-screen");
    expect(markup).toContain("bg-bg-primary");
    expect(markup).toContain("animate-spin");
  });

  it("renders the reconcile phase with counts when the memory index rebuilds during boot", () => {
    // 3/8 → 37.5% → a 38% fill — distinct from the indeterminate 30% stub,
    // so this pins the determinate bar, not just the label.
    const markup = renderToStaticMarkup(
      <BootSplash reconcile={{ phase: "running", done: 3, total: 8 }} />,
    );
    expect(markup).toContain("Building memory index");
    expect(markup).toContain("Reconciling memories…");
    expect(markup).toContain("3/8");
    expect(markup).toContain("width:38%");
  });

  it("renders the reconcile phase indeterminate before the first progress event", () => {
    const markup = renderToStaticMarkup(
      <BootSplash reconcile={{ phase: "running", done: 0, total: 0 }} />,
    );
    expect(markup).toContain("Building memory index");
    // Pin the mono counter span's content — a bare "…" toContain would be
    // trivially satisfied by the label ("Reconciling memories…").
    expect(markup).toContain('font-mono">…</span>');
    expect(markup).not.toContain("0/0");
  });

  it("renders the done hint (spinner kept) after the reconcile finishes", () => {
    const markup = renderToStaticMarkup(
      <BootSplash
        reconcile={{ phase: "done", summary: "derived index reconciled" }}
      />,
    );
    expect(markup).toContain("Memory index ready");
    expect(markup).toContain("animate-spin");
  });

  it("renders the failed hint when the reconcile fails", () => {
    const markup = renderToStaticMarkup(
      <BootSplash reconcile={{ phase: "failed", error: "boom" }} />,
    );
    expect(markup).toContain("Memory index check failed");
  });
});

describe("App boot-splash wiring (source contract, review LOW 2)", () => {
  it("the boot-splash early return sits before the picker return", () => {
    // The splash must cover the pre-index boot phases: if the early return
    // drifted below the picker/error returns, App's empty main-UI shell
    // would flash again (backlog 486955d5).
    const splashIdx = appSource.indexOf("if (!checkedStartup) {");
    const pickerIdx = appSource.indexOf(
      "if (checkedStartup && needsProject) {",
    );
    expect(splashIdx).toBeGreaterThan(-1);
    expect(pickerIdx).toBeGreaterThan(splashIdx);
  });

  it("the early return renders BootSplash with the tracked reconcile state", () => {
    // A dropped prop silently reverts the boot window to the generic
    // spinner — the slow-reconcile feedback gap would return unnoticed
    // (backlog 486955d5 completion).
    expect(appSource).toContain(
      "return <BootSplash reconcile={reconcile} />;",
    );
  });
});
