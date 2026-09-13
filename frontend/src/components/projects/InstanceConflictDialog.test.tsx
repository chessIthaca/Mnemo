// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * InstanceConflictDialog render + wiring test.
 *
 * This project has no React DOM test infra (vitest runs in the `node`
 * environment — see InflightBar.test.ts), so:
 * - `renderToStaticMarkup` asserts the rendered text (heading, incumbent
 *   pid, both actions) — a dialog that renders the claim but no exit would
 *   trap the user;
 * - a `?raw` source-contract check asserts both buttons are wired to their
 *   callbacks — a dialog that renders but cannot dismiss or switch is the
 *   exact failure this guards.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { InstanceConflictDialog } from "./InstanceConflictDialog";
import source from "./InstanceConflictDialog.tsx?raw";

const conflict = { pid: 4242, started_at: 1_700_000_000 };

function render() {
  return renderToStaticMarkup(
    <InstanceConflictDialog
      conflict={conflict}
      onDismiss={() => {}}
      onChooseProject={() => {}}
    />,
  );
}

describe("InstanceConflictDialog", () => {
  it("renders the warning with the incumbent pid", () => {
    const markup = render();
    expect(markup).toContain("This project is already open");
    expect(markup).toContain("Another mnemo instance");
    expect(markup).toContain("PID 4242");
  });

  it("offers both actions: choose another project, open anyway", () => {
    const markup = render();
    expect(markup).toContain("Choose another project");
    expect(markup).toContain("Open it anyway");
  });

  it("wires the buttons to their callbacks", () => {
    expect(source).toContain("onClick={onDismiss}");
    expect(source).toContain("onClick={onChooseProject}");
  });

  it("keeps the dialog a full-viewport modal (z-50 overlay, like other modals)", () => {
    expect(source).toContain("fixed inset-0 z-50");
  });
});
