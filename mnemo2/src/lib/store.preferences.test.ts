import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "./store";
import { fixtureState } from "../test/fixtures";
import type { ProjectPreferences } from "./projectEngine";

const { transport } = vi.hoisted(() => ({
  transport: {
    list: vi.fn(),
    add: vi.fn(),
    create: vi.fn(),
    pickFolder: vi.fn(),
    preferences: vi.fn(),
    savePreferences: vi.fn(),
    attach: vi.fn(),
    cleanup: vi.fn(),
  },
}));
vi.mock("./projectEngine", () => ({ createProjectTransport: () => transport }));
vi.mock("./runtime", async (original) => ({
  ...(await original()),
  probeMnemo2Host: async () => true,
}));

const saved: ProjectPreferences = {
  workbench_open: true,
  workbench_tab: "diff",
  workbench_width: 650,
  browser_url: "http://localhost:3000/",
  diff_worker_id: "ana",
};

describe("per-project workbench persistence", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    transport.preferences.mockResolvedValue(saved);
    transport.savePreferences.mockResolvedValue(undefined);
    transport.attach.mockResolvedValue({ conflict_pid: null, conflict_started_at: null });
    app.set(fixtureState());
  });

  it("restores the real preferences and saves against the originating project", async () => {
    const project = get(app).projects[0];
    await actions.openProject(project.id);
    await vi.waitFor(() => expect(get(app).workbenchWidth).toBe(650));
    expect(get(app).workbenchTab).toBe("diff");
    expect(get(app).diffWorkerId).toBe("ana");
    expect(get(app).browserUrl).toBe("http://localhost:3000/");

    actions.setWorkbenchWidth(500);
    await vi.waitFor(() => expect(transport.savePreferences).toHaveBeenCalled());
    expect(transport.savePreferences).toHaveBeenLastCalledWith(project.path, {
      ...saved,
      workbench_width: 500,
    });
    await actions.closeProject();
    expect(get(app).route).toBe("start");
  });
});
