import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LeadEvent, LeadSnapshot } from "./types";
import type { LeadTransport } from "./leadEngine";
import { emptyProjectUsageView } from "./teamState";

/**
 * Regression: opening a project must not race the automatic lead open.
 *
 * openProject used to start `connectNativeLead` and `connectTeam` in one
 * `Promise.all`, so the team snapshot/subscription ran while lead_open was
 * still in flight. The native host answers those dependent commands with
 * `Mnemo2 has no open lead; call lead_open first` until the lead exists, which
 * surfaced as a spurious toast on a perfectly normal project open. The
 * dependent startup must wait for the lead open to finish.
 */
const lead = vi.hoisted(() => ({
  transport: null as LeadTransport | null,
}));

const team = vi.hoisted(() => ({
  // The host rejects dependent IPC until lead_open has completed.
  leadOpen: false,
  subscriptions: 0,
  snapshots: 0,
}));

const NO_OPEN_LEAD = "Mnemo2 has no open lead; call lead_open first";

vi.mock("./leadEngine", () => ({
  createLeadTransport: () => lead.transport,
}));

vi.mock("./teamEngine", () => ({
  createTeamTransport: () => ({
    onEvent: async () => {
      team.subscriptions += 1;
      if (!team.leadOpen) throw new Error(NO_OPEN_LEAD);
      return () => {};
    },
    snapshot: async () => {
      team.snapshots += 1;
      if (!team.leadOpen) throw new Error(NO_OPEN_LEAD);
      return {
        workers: [],
        slots: 0,
        active: 0,
        usage: emptyProjectUsageView(),
      };
    },
    messages: async () => {
      if (!team.leadOpen) throw new Error(NO_OPEN_LEAD);
      return [];
    },
  }),
}));

vi.mock("./projectEngine", () => ({
  createProjectTransport: () => ({
    attach: async () => ({ conflict_pid: null, conflict_started_at: null }),
    preferences: async () => ({
      workbench_open: false,
      workbench_tab: "browser",
      workbench_width: 480,
      browser_url: "about:blank",
      diff_worker_id: null,
    }),
  }),
}));

vi.mock("./runtime", () => ({
  isTauriRuntime: () => true,
  probeMnemo2Host: async () => true,
}));

import { actions, app } from "./store";
import { fixtureState } from "../test/fixtures";

function snapshot(): LeadSnapshot {
  return {
    generation: 1,
    model: "test/scripted-a",
    model_id: "scripted-a",
    models: [
      {
        endpoint: "test",
        model: "scripted-a",
        spec: "test/scripted-a",
        is_default: true,
      },
    ],
    default_model: "test/scripted-a",
    transcript: { messages: [] },
    used_tokens: 0,
    max_tokens: 1000,
    pending: null,
    busy: false,
  };
}

function makeLeadTransport(openGate: Promise<void>): LeadTransport {
  return {
    probe: vi.fn(async () => true),
    open: vi.fn(async () => {
      await openGate;
      return snapshot();
    }),
    snapshot: vi.fn(async () => snapshot()),
    prompt: vi.fn(async () => {}),
    interrupt: vi.fn(async () => {}),
    setModel: vi.fn(async () => snapshot()),
    approve: vi.fn(async () => {}),
    answer: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    onEvent: vi.fn(async (_handler: (event: LeadEvent) => void) => () => {}),
  };
}

describe("Mnemo2 project startup order", () => {
  beforeEach(() => {
    lead.transport = null;
    team.leadOpen = false;
    team.subscriptions = 0;
    team.snapshots = 0;
    app.set(fixtureState());
  });

  it("project_open_waits_for_lead_before_dependent_ipc", async () => {
    let resolveOpen!: () => void;
    const openGate = new Promise<void>((resolve) => {
      resolveOpen = resolve;
    });
    const transport = makeLeadTransport(openGate);
    lead.transport = transport;

    const opening = actions.openProject("mnemo");
    await vi.waitFor(() => expect(transport.open).toHaveBeenCalled());

    // lead_open is still in flight: no dependent team IPC may have started.
    expect(get(app).leadRuntime.status).toBe("opening");
    expect(team.subscriptions).toBe(0);
    expect(team.snapshots).toBe(0);
    expect(get(app).toast).toBeNull();

    team.leadOpen = true;
    resolveOpen();
    await opening;

    // Once the lead is open, the dependent startup runs and stays quiet.
    expect(team.subscriptions).toBe(1);
    expect(team.snapshots).toBeGreaterThan(0);
    expect(get(app).leadRuntime.status).toBe("ready");
    expect(get(app).toast?.message ?? "").not.toMatch(/no open lead/i);
  });
});
