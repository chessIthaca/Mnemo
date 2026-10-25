import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import StartScreen from "./StartScreen.svelte";

const { transport } = vi.hoisted(() => ({
  transport: {
    list: vi.fn(),
    add: vi.fn(),
    create: vi.fn(),
    pickFolder: vi.fn(),
    attach: vi.fn(),
    preferences: vi.fn(),
    savePreferences: vi.fn(),
    cleanup: vi.fn(),
  },
}));
vi.mock("../lib/projectEngine", () => ({ createProjectTransport: () => transport }));
vi.mock("../lib/runtime", async (original) => ({
  ...(await original()),
  probeMnemo2Host: async () => true,
}));

const defaults = {
  workbench_open: false,
  workbench_tab: "browser" as const,
  workbench_width: 480,
  browser_url: "about:blank",
  diff_worker_id: null,
};

describe("project lifecycle", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    transport.list.mockImplementation(async () => get(app).projects);
    transport.preferences.mockResolvedValue(defaults);
    transport.attach.mockResolvedValue({ conflict_pid: null, conflict_started_at: null });
    transport.pickFolder.mockResolvedValue("C:/parent");
    transport.cleanup.mockResolvedValue({ removed: ["wt/agent-clean"], skipped: ["wt/agent-dirty"] });
    const state = fixtureState();
    state.projects[0].unfinishedWorkers = 2;
    state.projects[0].reviewWorkers = 2;
    state.projects = state.projects.map((project) => ({ ...project, id: project.path }));
    app.set(state);
  });

  it("confirms cleanup and reports actual removed and retained worktrees", async () => {
    const project = get(app).projects[0];
    render(StartScreen);
    await fireEvent.click(screen.getByRole("button", { name: "Clean" }));
    expect(screen.getByText(/Every worker branch is retained/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Clean worktrees" }));
    await vi.waitFor(() => expect(transport.cleanup).toHaveBeenCalledWith(project.path));
    expect(get(app).toast?.message).toContain("Removed 1 clean worktree");
    expect(get(app).toast?.message).toContain("retained 1 for review");
    expect(get(app).cleanupProjectId).toBeNull();
  });

  it("creates a real project through the native folder and registry flow", async () => {
    transport.create.mockResolvedValue({ name: "new", path: "C:/new" });
    transport.list.mockResolvedValue([{ name: "new", path: "C:/new" }]);
    render(StartScreen);
    await fireEvent.click(screen.getByRole("button", { name: "New project" }));
    const input = await screen.findByLabelText("Folder name");
    await fireEvent.input(input, { target: { value: "demo" } });
    await fireEvent.submit(input.closest("form")!);
    await vi.waitFor(() => expect(get(app).projectId).toBe("C:/new"));
    expect(transport.create).toHaveBeenCalledWith("C:/parent", "demo");
  });

  it("requires an explicit decision before opening a conflicting instance", async () => {
    const project = get(app).projects[0];
    transport.attach.mockResolvedValueOnce({ conflict_pid: 42, conflict_started_at: 7 });
    render(StartScreen);
    await actions.openProject(project.id);
    expect(await screen.findByText(/PID 42/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Open anyway" }));
    await vi.waitFor(() => expect(get(app).projectConflict).toBeNull());
    expect(transport.attach).toHaveBeenLastCalledWith(project.path, true);
  });
});
