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
 * - the occupancy contract: the copy is a LAUNCH-time claim and must never
 *   assert the incumbent is still working on the project (SPEC
 *   2027-01-11-same-project-instance-warning-is-a-launch-time-s.md — the
 *   conflict is a launch-time snapshot, re-checked live by App.tsx);
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
  it("renders the launch-time warning with the incumbent pid", () => {
    const markup = render();
    expect(markup).toContain("Another mnemo instance launched on this project");
    expect(markup).toContain("PID 4242");
    expect(markup).toContain("launched on this");
  });

  it("never asserts current occupancy (launch-time claim only)", () => {
    // The conflict is a launch-time snapshot: the incumbent may have exited
    // since (the dialog retracts live once it does — backlog ea9576d7). The
    // copy must never claim it is STILL working on the project.
    const markup = render();
    expect(markup).not.toContain("already open");
    expect(markup).not.toContain("already working");
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
