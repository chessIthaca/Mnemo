import { render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App.svelte";
import type { OpenWorkerRequest } from "./lib/workerSelection";

/**
 * Claude Code pointing Mnemo2 at a worker: the helper's `show-worker` makes
 * the host raise the window and emit `mnemo2://open-worker`. The host is a
 * scripted fake; the app and its store are the real ones.
 */
const host = vi.hoisted(() => ({
  native: true,
  handlers: [] as ((request: OpenWorkerRequest) => void)[],
  unlisten: vi.fn(),
}));

vi.mock("./lib/leadEngine", () => ({ createLeadTransport: () => null }));
vi.mock("./lib/teamEngine", () => ({ createTeamTransport: () => null }));
vi.mock("./lib/boardEngine", () => ({ createBoardTransport: () => null }));
vi.mock("./lib/projectEngine", () => ({ createProjectTransport: () => null }));
vi.mock("./lib/runtime", () => ({
  isTauriRuntime: () => true,
  probeMnemo2Host: async () => true,
}));
vi.mock("./lib/externalLeadEngine", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./lib/externalLeadEngine")>()),
  // Rendering the App starts the external-lead listeners too; keep them quiet.
  createExternalLeadTransport: () => null,
}));
vi.mock("./lib/workerSelection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./lib/workerSelection")>()),
  createOpenWorkerTransport: () =>
    host.native
      ? {
          onOpenWorker: async (handler: (request: OpenWorkerRequest) => void) => {
            host.handlers.push(handler);
            return host.unlisten;
          },
        }
      : null,
}));

import { actions, app, createInitialState, selectedWorker } from "./lib/store";
import { fixtureState } from "./test/fixtures";

const state = () => get(app);
// The lead view has its own status region ("Waiting on <worker>"), so the
// notice is told apart by its message, not its role.
const noticeOnScreen = () => screen.queryByText(/Claude Code asked to show a worker/);
const emitOpenWorker = (root: string, workerId: string) =>
  host.handlers.forEach((handler) => handler({ root, worker_id: workerId }));

describe("Mnemo2 showing the worker Claude Code names", () => {
  beforeEach(async () => {
    actions.resetDemo();
    host.native = true;
    host.handlers = [];
    host.unlisten.mockClear();
    app.set({ ...fixtureState(), route: "lead", selectedWorkerId: null });
    render(App);
    await actions.initOpenWorker();
  });

  it("opens mission control on the worker for the open project", async () => {
    emitOpenWorker("\\\\?\\C:\\AgenticCoder\\AgenticCoder\\", "kai");

    expect(await screen.findByRole("region", { name: "Mission Control" })).toBeTruthy();
    expect(state().route).toBe("team");
    expect(get(selectedWorker)?.id).toBe("kai");
    expect(noticeOnScreen()).toBeNull();
  });

  it("keeps the lead view and explains a request for another project", async () => {
    emitOpenWorker("C:/Games/Garden", "kai");

    const live = await screen.findByText(/Claude Code asked to show a worker in C:\/Games\/Garden/);
    expect(live.textContent).toContain("which is not open");
    expect(state().route).toBe("lead");
    expect(get(selectedWorker)).toBeNull();
    expect(screen.getByRole("textbox", { name: "Message the lead" })).toBeTruthy();
  });

  it("keeps the view and explains an unknown worker", async () => {
    emitOpenWorker("C:/AgenticCoder/AgenticCoder", "nope");

    const live = await screen.findByText(/Claude Code asked to show worker nope/);
    expect(live.textContent).toContain("Mnemo");
    expect(state().route).toBe("lead");
    expect(get(selectedWorker)).toBeNull();
  });

  it("explains itself when no project is open", async () => {
    app.set(createInitialState());

    emitOpenWorker("C:/AgenticCoder/AgenticCoder", "kai");

    const live = await screen.findByText(/no project is open/);
    expect(live.textContent).toContain("Claude Code asked to show a worker");
    expect(state().route).toBe("start");
    expect(get(selectedWorker)).toBeNull();
  });

  it("starts one listener, and only the live listener answers", async () => {
    await actions.initOpenWorker();
    expect(host.handlers).toHaveLength(1);

    emitOpenWorker("C:/AgenticCoder/AgenticCoder", "sol");
    expect(state().selectedWorkerId).toBe("sol");
    expect(host.unlisten).not.toHaveBeenCalled();

    // A reset disconnects: the old handler's token is dead, so its events do nothing.
    actions.resetDemo();
    expect(host.unlisten).toHaveBeenCalledTimes(1);
    emitOpenWorker("C:/AgenticCoder/AgenticCoder", "sol");
    expect(state().toast).toBeNull();

    await actions.initOpenWorker();
    expect(host.handlers).toHaveLength(2);
  });

  it("has nothing to listen to without the native shell", async () => {
    host.native = false;
    actions.resetDemo();
    const registered = host.handlers.length;

    await actions.initOpenWorker();
    expect(host.handlers).toHaveLength(registered);

    emitOpenWorker("C:/AgenticCoder/AgenticCoder", "kai");
    expect(state().toast).toBeNull();
    expect(state().projectId).toBeNull();
  });
});