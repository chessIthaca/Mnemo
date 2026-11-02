// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Add-agent form contract (plan 2c26d533): the tab-rail "+" button spawns a
 * new agent, optionally as a read-only review agent.
 *
 * Security anchor — the point of this suite: `reviewOnly: true` is the key
 * that asks the backend for the strict reviewer role (role "reviewer" →
 * REVIEWER_BASE_TOOLS). A frontend regression that drops the key (or
 * renames it) silently spawns a FULLY PRIVILEGED agent labelled
 * "reviewer" — the exact hazard the plan's backend step exists to prevent.
 * So the assertions pin the REAL `invoke("spawn_agent", { name, reviewOnly })`
 * payload (the wrapper runs for real on a mocked bridge), not a mock of the
 * wrapper.
 *
 * This project has no React DOM test infra (vitest runs in the `node`
 * environment — see InflightBar.test.ts), so the suite combines the repo's
 * established patterns:
 *  - a mocked Tauri `invoke` (`@tauri-apps/api/core`) with the REAL
 *    lib/tauri.ts `spawnAgent` wrapper running on top — only lib/tauri.ts
 *    imports from core, and only `invoke`, so a factory mock is the whole
 *    bridge;
 *  - `submitAddAgent` — the form's pure IPC + store seam, exported from
 *    MainPanel.tsx exactly so this node-env suite can drive the real
 *    spawn → register → activate chain (the openMarkdownTarget pattern in
 *    MarkdownLink.test.tsx);
 *  - renderToStaticMarkup over the real component (the "+" button + closed
 *    form render, and the "spawned agent's tab became ACTIVE" check) plus
 *    `?raw` source contracts for the form's React wiring that static
 *    rendering cannot reach (click opens, Enter submits, Esc cancels,
 *    busy-disabled, error-keeps-form-open — the InstanceConflictDialog.test.tsx
 *    style).
 */

import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

// The Tauri IPC bridge. vi.mock is hoisted above every import below, so the
// REAL lib/tauri.ts (imported via MainPanel.tsx) resolves `invoke` to this
// mock — the wrapper's payload logic runs for real.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import { useAgentStore } from "../../hooks/useAgentStore";
import type { AgentInfo } from "../../lib/types";
import { MainPanel, submitAddAgent } from "./MainPanel";
import mainPanelSource from "./MainPanel.tsx?raw";

/**
 * Reset the store slices the spawn path writes + reads (the ipc-contract
 * resetStore pattern) so each test starts from an empty agent registry.
 */
function resetStore(): void {
  useAgentStore.setState({
    agents: {},
    agentNames: {},
    agentParents: {},
    agentModels: {},
    agentProviders: {},
    agentWireEfforts: {},
    activeAgent: null,
  });
}

/** A backend-shaped AgentInfo for a freshly spawned (parentless) agent. */
function spawnedInfo(id: number, name: string): AgentInfo {
  return { id, name, running: false, parent_id: null };
}

/**
 * Static-render harness seam: renderToStaticMarkup reads
 * useSyncExternalStore's SERVER snapshot, and zustand v5 wires that to the
 * INTERNAL api's getInitialState() (the `api` object inside createImpl's
 * closure — Object.assign only COPIES getInitialState onto the imported
 * hook, so vi.spyOn on the hook cannot intercept what React actually
 * calls). But getInitialState() returns the initial-state OBJECT itself,
 * so mirroring the current fields onto it makes the static render reflect
 * them — the node-env equivalent of seeding before import
 * (RightPanel.width.test.tsx). Call it after every store mutation the
 * render must see.
 */
function syncInitialStateForStaticRender(): void {
  Object.assign(useAgentStore.getInitialState(), useAgentStore.getState());
}

describe("submitAddAgent — the spawn_agent IPC payload + register/activate wiring", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    resetStore();
  });

  it("Create with the box CHECKED invokes spawn_agent with { name, reviewOnly: true }", async () => {
    vi.mocked(invoke).mockResolvedValue(spawnedInfo(7, "reviewer-1"));
    const res = await submitAddAgent("reviewer-1", true);
    expect(res.ok).toBe(true);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("spawn_agent", {
      name: "reviewer-1",
      reviewOnly: true,
    });
  });

  it("Create with the box UNCHECKED sends reviewOnly: false", async () => {
    vi.mocked(invoke).mockResolvedValue(spawnedInfo(8, "worker-1"));
    const res = await submitAddAgent("worker-1", false);
    expect(res.ok).toBe(true);
    expect(invoke).toHaveBeenCalledWith("spawn_agent", {
      name: "worker-1",
      reviewOnly: false,
    });
  });

  it("trims the name before sending it", async () => {
    vi.mocked(invoke).mockResolvedValue(spawnedInfo(9, "trimmed"));
    await submitAddAgent("  spaced out  ", false);
    expect(invoke).toHaveBeenCalledWith("spawn_agent", {
      name: "spaced out",
      reviewOnly: false,
    });
  });

  it("fails fast on a blank name without any IPC call", async () => {
    const res = await submitAddAgent("   ", false);
    expect(res.ok).toBe(false);
    expect(res.error).toBe("Agent name is required");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("registers the new agent in the SAME registry runtime spawns use and activates its tab", async () => {
    vi.mocked(invoke).mockResolvedValue(spawnedInfo(11, "rev-2"));
    const res = await submitAddAgent("rev-2", true);
    expect(res.ok).toBe(true);
    expect(res.agentId).toBe(11);
    const s = useAgentStore.getState();
    // activeAgent === the returned id is "the returned agent becomes
    // active"...
    expect(s.activeAgent).toBe(11);
    // ...and it went through the single shared registry — the same slices
    // useAgentEvents fills for runtime-spawned agents (registerAgents),
    // never a parallel bookkeeping path.
    expect(s.agentNames[11]).toBe("rev-2");
    expect(s.agentParents[11]).toBeNull();
    expect(s.agents[11]).toBeDefined();
  });

  it("a rejected invoke surfaces errMsg's message and touches NOTHING in the store (form stays open)", async () => {
    vi.mocked(invoke).mockRejectedValue({
      kind: "error",
      message: "an agent with that name already exists",
    });
    const res = await submitAddAgent("dup", true);
    expect(res.ok).toBe(false);
    expect(res.agentId).toBeNull();
    // errMsg extracted the structured IpcError DTO's message — the inline
    // form shows the backend's reason, not "[object Object]" (tauri.test.ts
    // pins errMsg itself; this pins the seam using it).
    expect(res.error).toBe("an agent with that name already exists");
    const s = useAgentStore.getState();
    expect(s.activeAgent).toBeNull();
    expect(s.agents).toEqual({});
    expect(invoke).toHaveBeenCalledTimes(1);
  });
});

describe("the tab-rail render (static markup over the real component + store)", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    resetStore();
    // Keep the static render's read path (the store's initial state —
    // see syncInitialStateForStaticRender) in sync with the reset.
    syncInitialStateForStaticRender();
  });

  it("renders the Add agent button at the end of the rail, with the form closed", () => {
    const markup = renderToStaticMarkup(<MainPanel />);
    // The "+" affordance is present on an otherwise empty rail...
    expect(markup).toContain('aria-label="Add agent"');
    expect(markup).toContain("No agents");
    // ...and the form is closed: its checkbox and its form row never
    // rendered.
    expect(markup).not.toContain("Read-only review agent");
    expect(markup).not.toContain('aria-label="Add agent form"');
  });

  it("after a successful spawn the returned agent's tab renders as the ACTIVE tab", async () => {
    vi.mocked(invoke).mockResolvedValue(spawnedInfo(12, "rev-3"));
    const res = await submitAddAgent("rev-3", true);
    expect(res.ok).toBe(true);
    // Make the spawn's store mutations visible to the static render
    // (see syncInitialStateForStaticRender).
    syncInitialStateForStaticRender();
    const markup = renderToStaticMarkup(<MainPanel />);
    expect(markup).toContain("rev-3");
    // Radix marks the selected trigger with aria-selected — the new agent
    // is not merely present, its tab became the active one.
    expect(markup).toContain('aria-selected="true"');
    // And the form closed after success (its fields never rendered).
    expect(markup).not.toContain('aria-label="Add agent form"');
  });
});

describe("form wiring — source contracts (the node env cannot click)", () => {
  it("clicking Add agent opens the form (and clears any stale error)", () => {
    expect(mainPanelSource).toContain("setAddOpen(true)");
    expect(mainPanelSource).toContain("{addOpen && (");
  });

  it("Enter submits through the form's onSubmit → submitAddAgent(name, reviewOnly)", () => {
    expect(mainPanelSource).toContain("void submitAdd();");
    expect(mainPanelSource).toContain("submitAddAgent(addName, addReviewOnly)");
  });

  it("Esc cancels — closes and resets the form", () => {
    expect(mainPanelSource).toContain('if (e.key === "Escape") {');
  });

  it("Create is disabled while a spawn is in flight or the name is blank after trimming", () => {
    expect(mainPanelSource).toContain("disabled={addBusy || !addName.trim()}");
  });

  it("only success closes the form; failure shows the error inline and keeps the inputs intact", () => {
    expect(mainPanelSource).toContain("if (res.ok) {");
    expect(mainPanelSource).toContain("setAddError(res.error");
    expect(mainPanelSource).toContain('role="alert"');
  });

  it("the + button is a PLAIN button, a sibling of the TabsTrigger elements (no nested buttons)", () => {
    // The "+" must sit inside the TabsList but OUTSIDE every TabsTrigger
    // (the f3dd412 nested-button fix, and Radix arrow-key navigation skips
    // it because it is not part of the tab collection). Pin the order in
    // the source: after the LAST trigger close, before the list close.
    const lastTriggerClose = mainPanelSource.lastIndexOf("</TabsTrigger>");
    const buttonIdx = mainPanelSource.indexOf('aria-label="Add agent"');
    const listClose = mainPanelSource.indexOf("</TabsList>");
    expect(lastTriggerClose).toBeGreaterThan(-1);
    expect(buttonIdx).toBeGreaterThan(lastTriggerClose);
    expect(buttonIdx).toBeLessThan(listClose);
  });
});