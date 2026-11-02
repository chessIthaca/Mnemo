import { render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ExternalLeadEvent, ExternalLeadStatus, ExternalOpenRequest } from "./externalLeadEngine";
import type { ProjectRecord } from "./projectEngine";
import type { LeadSnapshot } from "./types";

/**
 * Claude Code leading a project from outside: the startup `--project`, open
 * requests over the event channel, and the attach/detach state that drives the
 * top-bar badge. The native host is a scripted fake.
 */
const host = vi.hoisted(() => ({
  native: true,
  records: [] as ProjectRecord[],
  /** Registry reads that should still come back without the new project. */
  staleReads: 0,
  list: vi.fn(),
  attach: vi.fn(),
  startup: null as string | null,
  status: { attached: false, root: null, since_ms: null } as ExternalLeadStatus,
  statusError: false,
  /** Holds every status read until it resolves. */
  statusGate: null as Promise<void> | null,
  statusAsked: vi.fn(),
  reportOpenFailure: vi.fn(async (_root: string, _reason: string) => undefined),
  /** Whether the native lead transport exists, and what opening the lead does. */
  leadNative: true,
  leadOpen: vi.fn(),
  openHandlers: [] as ((request: ExternalOpenRequest) => void)[],
  changeHandlers: [] as ((event: ExternalLeadEvent) => void)[],
  unlisten: vi.fn(),
}));

vi.mock("./leadEngine", () => ({
  createLeadTransport: () =>
    host.leadNative
      ? {
          probe: async () => true,
          open: host.leadOpen,
          close: async () => undefined,
          onEvent: async () => () => undefined,
        }
      : null,
}));
vi.mock("./teamEngine", () => ({ createTeamTransport: () => null }));
vi.mock("./boardEngine", () => ({ createBoardTransport: () => null }));
vi.mock("./indexingEngine", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./indexingEngine")>()),
  createIndexingTransport: () => null,
}));
vi.mock("./projectEngine", () => ({
  createProjectTransport: () => ({
    list: host.list,
    attach: host.attach,
    preferences: async () => ({
      workbench_open: false,
      workbench_tab: "browser",
      workbench_width: 480,
      browser_url: "about:blank",
      diff_worker_id: null,
  board_width: null,
    }),
  }),
}));
vi.mock("./runtime", () => ({
  isTauriRuntime: () => true,
  probeMnemo2Host: async () => true,
}));
vi.mock("./externalLeadEngine", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./externalLeadEngine")>()),
  createExternalLeadTransport: () =>
    host.native
      ? {
          // Like the host: the startup project is handed out once.
          startupProject: async () => {
            const root = host.startup;
            host.startup = null;
            return root;
          },
          status: async () => {
            host.statusAsked();
            if (host.statusError) throw new Error("Unknown command external_lead_status");
            const reply = host.status;
            if (host.statusGate) await host.statusGate;
            return reply;
          },
          onOpenRequest: async (handler: (request: ExternalOpenRequest) => void) => {
            host.openHandlers.push(handler);
            return host.unlisten;
          },
          onChanged: async (handler: (event: ExternalLeadEvent) => void) => {
            host.changeHandlers.push(handler);
            return host.unlisten;
          },
          reportOpenFailure: host.reportOpenFailure,
        }
      : null,
}));

import TopBar from "../components/TopBar.svelte";
import { actions, app, createInitialState } from "./store";

const leadSnapshot: LeadSnapshot = {
  generation: 1,
  model: "test/scripted-a",
  model_id: "scripted-a",
  models: [],
  default_model: "test/scripted-a",
  transcript: { messages: [] },
  used_tokens: 0,
  max_tokens: 1000,
  pending: null,
  busy: false,
};

const mnemo: ProjectRecord = { name: "mnemo", path: "C:\\Mnemo", branch: "main" };
const garden: ProjectRecord = { name: "garden", path: "C:\\Garden", branch: "main" };

const state = () => get(app);
const toast = () => state().toast?.message ?? null;
const emitOpen = (root: string) => host.openHandlers.forEach((handler) => handler({ root }));
const emitLead = (event: ExternalLeadEvent) => host.changeHandlers.forEach((handler) => handler(event));
const badge = () => screen.queryByText("Lead: Claude Code (external)");

describe("Claude Code opening a project", () => {
  beforeEach(() => {
    actions.resetDemo();
    app.set(createInitialState());
    host.native = true;
    host.records = [mnemo, garden];
    host.staleReads = 0;
    host.list.mockReset();
    host.list.mockImplementation(async () =>
      host.staleReads-- > 0 ? [garden] : host.records,
    );
    host.attach.mockReset();
    host.attach.mockResolvedValue({ conflict_pid: null, conflict_started_at: null });
    host.leadNative = true;
    host.leadOpen.mockReset();
    host.leadOpen.mockResolvedValue(leadSnapshot);
    host.startup = null;
    host.status = { attached: false, root: null, since_ms: null };
    host.statusError = false;
    host.statusGate = null;
    host.statusAsked.mockReset();
    host.reportOpenFailure.mockClear();
    host.openHandlers = [];
    host.changeHandlers = [];
    host.unlisten.mockReset();
  });

  it("opens the registered project the app was started with", async () => {
    // The host canonicalizes: verbatim prefix, other case, trailing slash.
    host.startup = "\\\\?\\c:\\mnemo\\";
    await actions.initExternalLead();

    expect(host.attach).toHaveBeenCalledWith("C:\\Mnemo", false);
    expect(state().projectId).toBe("C:\\Mnemo");
    expect(state().route).toBe("lead");
    expect(state().leadRuntime.status).toBe("ready");
    expect(toast()).toBeNull();
    expect(host.reportOpenFailure).not.toHaveBeenCalled();
  });

  it("starts on the project list when the app was started without a project", async () => {
    await actions.initExternalLead();

    expect(host.attach).not.toHaveBeenCalled();
    expect(state().projects.map((project) => project.name)).toEqual(["mnemo", "garden"]);
    expect(state().route).toBe("start");
  });

  it("toasts and opens nothing when the startup project is not registered", async () => {
    host.startup = "C:\\Elsewhere\\Thing";
    await actions.initExternalLead();

    expect(host.attach).not.toHaveBeenCalled();
    expect(state().projectId).toBeNull();
    expect(toast()).toBe(
      "Claude Code asked to open C:\\Elsewhere\\Thing, which is not a Mnemo2 project. Add it first.",
    );
    // The registry is never changed on Claude Code's behalf.
    expect(state().projects.map((project) => project.name)).toEqual(["mnemo", "garden"]);
    // The hidden window's toast reaches nobody, so the host learns why.
    expect(host.reportOpenFailure).toHaveBeenCalledWith(
      "C:\\Elsewhere\\Thing",
      "it is not a registered Mnemo2 project",
    );
  });

  it("opens the project an external-open-request names", async () => {
    await actions.initExternalLead();
    expect(host.openHandlers).toHaveLength(1);

    emitOpen("C:/Garden");
    await vi.waitFor(() => expect(state().projectId).toBe("C:\\Garden"));
    expect(host.attach).toHaveBeenCalledWith("C:\\Garden", false);
    await vi.waitFor(() => expect(state().leadRuntime.status).toBe("ready"));
    expect(host.reportOpenFailure).not.toHaveBeenCalled();
  });

  it("toasts for an open request naming an unregistered folder", async () => {
    await actions.initExternalLead();

    emitOpen("C:/Elsewhere/Thing");
    await vi.waitFor(() => expect(toast()).toMatch(/not a Mnemo2 project\. Add it first\.$/));
    expect(toast()).toContain("C:/Elsewhere/Thing");
    expect(host.attach).not.toHaveBeenCalled();
    expect(state().projectId).toBeNull();
  });

  it("re-reads the registry for a project added after the list loaded", async () => {
    host.staleReads = 1;
    await actions.initExternalLead();
    expect(state().projects.map((project) => project.name)).toEqual(["garden"]);

    emitOpen("C:/Mnemo");
    await vi.waitFor(() => expect(state().projectId).toBe("C:\\Mnemo"));
    expect(toast()).toBeNull();
  });

  it("leaves an already open project alone", async () => {
    await actions.initExternalLead();
    await actions.openProject("C:\\Mnemo");
    host.attach.mockClear();

    emitOpen("c:/mnemo");
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(host.attach).not.toHaveBeenCalled();
    expect(state().projectId).toBe("C:\\Mnemo");
    expect(toast()).toBeNull();
    expect(host.reportOpenFailure).not.toHaveBeenCalled();
  });

  it("reports a lead that failed to open, with its error", async () => {
    host.leadOpen.mockRejectedValue(new Error("could not start the headless engine: not_configured"));
    host.startup = "C:\\Mnemo";
    await actions.initExternalLead();

    // The project stays selected, with the error on its lead.
    expect(state().projectId).toBe("C:\\Mnemo");
    expect(state().leadRuntime.status).toBe("error");
    expect(host.reportOpenFailure).toHaveBeenCalledTimes(1);
    expect(host.reportOpenFailure).toHaveBeenCalledWith(
      "C:\\Mnemo",
      "could not start the headless engine: not_configured",
    );
  });

  it("reports a lead that never started", async () => {
    host.leadNative = false;
    host.startup = "C:\\Mnemo";
    await actions.initExternalLead();

    expect(state().projectId).toBe("C:\\Mnemo");
    expect(host.reportOpenFailure).toHaveBeenCalledWith("C:\\Mnemo", "the native lead did not start");
  });

  it("reports a project another Mnemo2 process is using, with its pid", async () => {
    await actions.initExternalLead();
    host.attach.mockResolvedValue({ conflict_pid: 4242, conflict_started_at: 1_700_000_000_000 });

    emitOpen("C:/Garden");
    await vi.waitFor(() => expect(host.reportOpenFailure).toHaveBeenCalledTimes(1));
    expect(host.reportOpenFailure).toHaveBeenCalledWith(
      "C:/Garden",
      "another Mnemo2 process (pid 4242) is using the project",
    );
    expect(state().projectId).toBeNull();
  });

  it("retries a project whose lead failed to open when asked again", async () => {
    host.leadOpen.mockRejectedValueOnce(new Error("not_configured"));
    host.startup = "C:\\Mnemo";
    await actions.initExternalLead();
    expect(state().leadRuntime.status).toBe("error");
    expect(host.reportOpenFailure).toHaveBeenCalledTimes(1);
    expect(host.attach).toHaveBeenCalledTimes(1);

    // The user fixed the cause; the helper asks again.
    emitOpen("C:/Mnemo");
    await vi.waitFor(() => expect(state().leadRuntime.status).toBe("ready"));
    expect(host.attach).toHaveBeenCalledTimes(2);
    expect(host.leadOpen).toHaveBeenCalledTimes(2);
    expect(state().projectId).toBe("C:\\Mnemo");
    expect(host.reportOpenFailure).toHaveBeenCalledTimes(1);
  });

  it("reports again when the retry fails too", async () => {
    host.leadOpen.mockRejectedValueOnce(new Error("first failure"));
    host.leadOpen.mockRejectedValueOnce(new Error("second failure"));
    host.startup = "C:\\Mnemo";
    await actions.initExternalLead();

    emitOpen("C:/Mnemo");
    await vi.waitFor(() => expect(host.reportOpenFailure).toHaveBeenCalledTimes(2));
    expect(host.reportOpenFailure).toHaveBeenLastCalledWith("C:/Mnemo", "second failure");
    expect(state().leadRuntime.status).toBe("error");
  });

  it("does not switch away from a different open project", async () => {
    await actions.initExternalLead();
    await actions.openProject("C:\\Mnemo");
    host.attach.mockClear();

    emitOpen("C:/Garden");
    await vi.waitFor(() => expect(toast()).toMatch(/another project is open/));
    expect(host.attach).not.toHaveBeenCalled();
    expect(state().projectId).toBe("C:\\Mnemo");
  });

  it("listens once however often it is started", async () => {
    await actions.initExternalLead();
    await actions.initExternalLead();

    expect(host.openHandlers).toHaveLength(1);
    expect(host.changeHandlers).toHaveLength(1);
  });

  it("does nothing without a native shell", async () => {
    host.native = false;
    await actions.initExternalLead();

    expect(host.list).not.toHaveBeenCalled();
    expect(host.openHandlers).toHaveLength(0);
    expect(state().projects).toEqual([]);
  });
});

describe("external lead state", () => {
  beforeEach(async () => {
    actions.resetDemo();
    app.set(createInitialState());
    host.native = true;
    host.records = [mnemo, garden];
    host.staleReads = 0;
    host.list.mockReset();
    host.list.mockImplementation(async () => host.records);
    host.attach.mockReset();
    host.attach.mockResolvedValue({ conflict_pid: null, conflict_started_at: null });
    host.leadNative = true;
    host.leadOpen.mockReset();
    host.leadOpen.mockResolvedValue(leadSnapshot);
    host.startup = null;
    host.status = { attached: false, root: null, since_ms: null };
    host.statusError = false;
    host.statusGate = null;
    host.statusAsked.mockReset();
    host.openHandlers = [];
    host.changeHandlers = [];
    host.unlisten.mockReset();
    await actions.initExternalLead();
  });

  it("starts detached", async () => {
    await actions.openProject("C:\\Mnemo");
    expect(state().externalLead).toEqual({ attached: false, root: null, sinceMs: null });
  });

  it("reads an attachment that exists when the project opens", async () => {
    host.status = { attached: true, root: "C:\\Mnemo", since_ms: 1_700_000_000_000 };
    await actions.openProject("C:\\Mnemo");

    expect(state().externalLead).toEqual({
      attached: true,
      root: "C:\\Mnemo",
      sinceMs: 1_700_000_000_000,
    });
  });

  it("ignores a status that names another project", async () => {
    host.status = { attached: true, root: "C:\\Garden", since_ms: 1 };
    await actions.openProject("C:\\Mnemo");

    expect(state().externalLead.attached).toBe(false);
  });

  it("stays detached, quietly, on a host without external-lead status", async () => {
    host.statusError = true;
    await actions.openProject("C:\\Mnemo");

    expect(state().externalLead.attached).toBe(false);
    expect(toast()).toBeNull();
  });

  it("follows attach and detach events for the open project", async () => {
    await actions.openProject("C:\\Mnemo");

    emitLead({ attached: true, root: "c:/mnemo" });
    expect(state().externalLead.attached).toBe(true);
    expect(state().externalLead.root).toBe("c:/mnemo");
    expect(state().externalLead.sinceMs).toEqual(expect.any(Number));

    emitLead({ attached: false, root: "c:/mnemo" });
    expect(state().externalLead).toEqual({ attached: false, root: null, sinceMs: null });
  });

  it("ignores events for a project that is not open", async () => {
    await actions.openProject("C:\\Mnemo");

    emitLead({ attached: true, root: "C:/Garden" });
    expect(state().externalLead.attached).toBe(false);

    emitLead({ attached: true, root: "C:/Mnemo" });
    emitLead({ attached: false, root: "C:/Garden" });
    expect(state().externalLead.attached).toBe(true);
  });

  it("ignores events while no project is open", () => {
    emitLead({ attached: true, root: "C:/Mnemo" });
    expect(state().externalLead.attached).toBe(false);
  });

  it("does not let a slower status read overwrite a newer event", async () => {
    let release!: () => void;
    host.statusGate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const opening = actions.openProject("C:\\Mnemo");
    await vi.waitFor(() => expect(host.statusAsked).toHaveBeenCalled());

    // The host attached after answering the status read.
    emitLead({ attached: true, root: "C:/Mnemo" });
    release();
    await opening;

    expect(state().externalLead.attached).toBe(true);
  });

  it("clears when the project closes or another opens", async () => {
    host.status = { attached: true, root: "C:\\Mnemo", since_ms: 1 };
    await actions.openProject("C:\\Mnemo");
    expect(state().externalLead.attached).toBe(true);

    host.status = { attached: false, root: null, since_ms: null };
    await actions.openProject("C:\\Garden");
    expect(state().externalLead.attached).toBe(false);

    emitLead({ attached: true, root: "C:/Garden" });
    await actions.closeProject();
    expect(state().projectId).toBeNull();
    expect(state().externalLead.attached).toBe(false);
  });

  it("shows the top-bar badge only while Claude Code is attached to the open project", async () => {
    await actions.openProject("C:\\Mnemo");
    render(TopBar);
    expect(badge()).toBeNull();

    emitLead({ attached: true, root: "C:/Garden" });
    await Promise.resolve();
    expect(badge()).toBeNull();

    emitLead({ attached: true, root: "C:/Mnemo" });
    expect(await screen.findByText("Lead: Claude Code (external)")).toBeTruthy();

    emitLead({ attached: false, root: "C:/Mnemo" });
    await vi.waitFor(() => expect(badge()).toBeNull());
  });
});
