import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * Regression: a Tauri WebView is not necessarily the Mnemo2 host.
 *
 * Mnemo1's Browser tab is a Tauri WebView too, so Mnemo2's dev UI opened
 * there used to send `project_pick_folder` to Mnemo1's IPC, which never
 * registered it — Tauri answered `project_pick_folder not allowed by ACL`
 * (the page origin is non-local for Mnemo1, so the ACL check applies).
 * The store must confirm the Mnemo2 handshake before invoking any command,
 * and the team transport needs the same gate.
 */
const host = vi.hoisted(() => ({
  handshake: false,
  pickCalls: 0,
  listCalls: 0,
  teamSubscriptions: 0,
}));

vi.mock("./runtime", () => ({
  isTauriRuntime: () => true,
  probeMnemo2Host: async () => host.handshake,
}));

vi.mock("./projectEngine", () => ({
  createProjectTransport: () => ({
    list: async () => {
      host.listCalls += 1;
      return [{ name: "picked", path: "C:/picked" }];
    },
    add: async (path: string) => ({ name: "picked", path }),
    pickFolder: async () => {
      host.pickCalls += 1;
      if (!host.handshake) {
        throw new Error("Command project_pick_folder not allowed by ACL");
      }
      return "C:/picked";
    },
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

vi.mock("./leadEngine", () => ({ createLeadTransport: () => null }));

vi.mock("./teamEngine", () => ({
  createTeamTransport: () => ({
    onEvent: async () => {
      host.teamSubscriptions += 1;
      return () => {};
    },
    snapshot: async () => ({ workers: [], slots: 0, active: 0 }),
    messages: async () => [],
    message: async () => {},
    dispatch: async () => {},
    stop: async () => {},
    pause: async () => {},
    resume: async () => {},
  }),
}));

import { actions, app, createInitialState } from "./store";

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("project and team commands need the Mnemo2 host, not just any Tauri WebView", () => {
  beforeEach(() => {
    host.handshake = false;
    host.pickCalls = 0;
    host.listCalls = 0;
    host.teamSubscriptions = 0;
    app.set(createInitialState());
  });

  it("refuses the folder picker on a foreign Tauri host", async () => {
    await actions.openFolder();

    expect(host.pickCalls).toBe(0);
    expect(host.listCalls).toBe(0);
    expect(get(app).toast?.message).toMatch(/native shell/i);
    expect(get(app).route).toBe("start");
  });

  it("opens the folder once the Mnemo2 host answers the handshake", async () => {
    host.handshake = true;

    await actions.openFolder();

    expect(host.pickCalls).toBe(1);
    expect(get(app).route).toBe("lead");
    expect(get(app).projectId).toBe("C:/picked");
  });

  it("gates the team transport on the same handshake", async () => {
    host.handshake = true;
    await actions.loadProjects();

    host.handshake = false;
    await actions.openProject("C:/picked");
    await settle();
    expect(host.teamSubscriptions).toBe(0);

    host.handshake = true;
    await actions.openProject("C:/picked");
    await settle();
    expect(host.teamSubscriptions).toBe(1);
  });
});
