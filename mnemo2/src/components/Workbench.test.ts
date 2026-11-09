import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { seedFilesPreview } from "../lib/files";
import { seedGitPreview } from "../lib/git";
import { seedTracePagePreview, type TraceRow } from "../lib/trace";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import Workbench from "./Workbench.svelte";

const state = () => get(app);

describe("Workbench", () => {
  beforeEach(() => {
    const state = fixtureState();
    state.board = [{
      id: "contract",
      title: "Define the lead/team contract",
      goal: "Build the board view",
      kind: "feature",
      status: "open",
      closed_at_ms: null,
      report_path: null,
      steps: [{ number: 1, header: "Contract", text: "Write it", done: false, workers: [] }],
    }];
    app.set(state);
    actions.openWorkbench("diff");
  });

  it("switches between the Diff, Plan, and Browser artifacts", async () => {
    render(Workbench);
    expect(screen.getByText("mnemo2/src/lib/review.ts")).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Plan" }));
    expect(await screen.findByRole("heading", { name: "Define the lead/team contract", level: 2 })).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Browser" }));
    expect(screen.getByRole("textbox", { name: "Address" })).toBeTruthy();
  });

  it("opens a lead-chat link in the browser and normalizes addresses", async () => {
    actions.openBrowser("https://example.com/docs");
    render(Workbench);

    const address = screen.getByRole("textbox", { name: "Address" }) as HTMLInputElement;
    expect(address.value).toBe("https://example.com/docs");

    await fireEvent.input(address, { target: { value: "example.com/notes" } });
    app.update((state) => ({ ...state, browserSupported: false }));
    expect(address.value).toBe("example.com/notes");
    await fireEvent.submit(address.closest("form")!);
    await vi.waitFor(() => expect(state().browserUrl).toBe("https://example.com/notes"));
  });

  it("routes plan-goal links through the workbench browser", async () => {
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) =>
      window.setTimeout(callback, 0));
    vi.stubGlobal("cancelAnimationFrame", (id: number) => window.clearTimeout(id));
    const current = fixtureState();
    current.board = [{
      id: "contract",
      title: "Plan",
      goal: "Read [Docs](https://example.com/docs)",
      kind: "feature",
      status: "open",
      closed_at_ms: null,
      report_path: null,
      steps: [],
    }];
    app.set(current);
    actions.openWorkbench("plan");
    render(Workbench);

    const link = await screen.findByRole("link", { name: "Docs" }, { timeout: 3000 });
    await fireEvent.click(link);
    expect(state().workbenchTab).toBe("browser");
    expect(state().browserUrl).toBe("https://example.com/docs");
  });

  it("shows only open plans in its Plan tab and links to the full list", async () => {
    const current = fixtureState();
    current.board = [
      {
        id: "contract",
        title: "Open contract",
        goal: "",
        kind: "feature",
        status: "open",
        closed_at_ms: null,
        report_path: null,
        steps: [],
      },
      {
        id: "done",
        title: "Finished plan",
        goal: "",
        kind: "bug_fixing",
        status: "closed",
        closed_at_ms: 5,
        report_path: null,
        steps: [],
      },
    ];
    app.set(current);
    actions.openWorkbench("plan");
    const { container } = render(Workbench);

    expect(await screen.findByRole("heading", { name: "Open contract", level: 2 })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Finished plan" })).toBeNull();
    expect(container.querySelector("details.closed")).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: "All plans" }));
    expect(state().route).toBe("plans");
    expect(state().workbenchOpen).toBe(false);
  });

  it("gives width back to the lead when the window shrinks", async () => {
    const original = Object.getOwnPropertyDescriptor(window, "innerWidth");
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1600 });
    try {
      actions.setWorkbenchWidth(1100);
      render(Workbench);
      expect(state().workbenchWidth).toBe(1100);

      Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1280 });
      await fireEvent(window, new Event("resize"));
      await vi.waitFor(() => expect(state().workbenchWidth).toBe(860));
    } finally {
      if (original) Object.defineProperty(window, "innerWidth", original);
    }
  });

  it("appends the Files tab to the strip and opens the project tree", async () => {
    seedFilesPreview({
      list: async (path: string) => ({
        path,
        entries: path === "" ? [{ path: "src", name: "src", kind: "dir" as const }] : [],
        truncated: false,
      }),
      read: async (path: string) => ({ path, text: "", bytes: 0, truncated: false, binary: false }),
      find: async () => [],
    });
    actions.openWorkbench("files");
    render(Workbench);

    const strip = screen.getByRole("navigation", { name: "Workbench tabs" });
    const labels = within(strip).getAllByRole("button").map((button) => button.textContent?.trim());
    expect(labels).toEqual(["Browser", "Diff", "Plan", "Files", "Git", "Trace", "Graph", "Memory"]);
    expect(within(strip).getByRole("button", { name: "Files" }).getAttribute("aria-pressed")).toBe("true");

    // The body switch renders the tree, which lists only the project root.
    expect(await screen.findByRole("button", { name: "src" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Files" })).toBeTruthy();
  });

  it("opens the Trace tab in the strip and lists the shared request log", async () => {
    const row: TraceRow = {
      id: 7,
      ts_ms: 0,
      model: "worker-model",
      base_url: "https://api.example.invalid/v1",
      provider: "primary",
      http_status: 400,
      usage: null,
      finish_reason: null,
      is_complete: true,
      version: 2,
      ttft_ms: 120,
      generation_ms: 120,
      reasoning_ms: null,
      connect_ms: null,
      upload_ms: null,
      server_ms: null,
      tools_ms: null,
      prep_ms: null,
      compact_ms: null,
      backoff_ms: null,
      stall_ms: null,
      chunks: null,
      error: "unexpected keyword 'strict' in tool schema",
      cancelled: false,
      response_truncated: false,
      response_evicted: false,
      guard_cut: null,
      run_id: "run-1",
      attribution: null,
      cost_micros: null,
      priced: false,
      retained: false,
    };
    seedTracePagePreview({
      list: async () => ({ rows: [row], total: 1, unpriced: 1, has_more: false, next_cursor: null, capped: false }),
      detail: async () => null,
      settings: async () => ({ log_bodies: false, ring_capacity: 128, journal_bytes: 0, history_bytes: 0 }),
      setLogBodies: async () => ({ log_bodies: false, ring_capacity: 128, journal_bytes: 0, history_bytes: 0 }),
      clearLive: async () => undefined,
      deleteHistory: async () => ({ deleted_files: 0, bytes: 0 }),
      exportCalls: async () => ({ path: "", count: 0, includes_bodies: false }),
      revealExport: async () => undefined,
    });
    actions.openWorkbench("trace");
    render(Workbench);

    const strip = screen.getByRole("navigation", { name: "Workbench tabs" });
    const labels = within(strip).getAllByRole("button").map((button) => button.textContent?.trim());
    expect(labels).toEqual(["Browser", "Diff", "Plan", "Files", "Git", "Trace", "Graph", "Memory"]);
    expect(within(strip).getByRole("button", { name: "Trace" }).getAttribute("aria-pressed")).toBe("true");

    // The panel hosts the full Trace view now, not the old compact summary.
    // A first dynamic import can outlast the default wait under full-suite load.
    expect(await screen.findByRole("region", { name: "Trace" }, { timeout: 15000 })).toBeTruthy();
    expect(await screen.findByRole("button", { name: /worker-model/ }, { timeout: 15000 })).toBeTruthy();
    expect(state().workbenchTab).toBe("trace");
  });
  it("appends the Git tab to the strip and fast-forwards main after confirmation", async () => {
    const moved: string[] = [];
    let main = "1111111";
    seedGitPreview({
      overview: async () => ({
        branch: "work",
        main,
        main_worktree: null,
        main_dirty: false,
        branches: [
          { name: "work", current: true, ahead: 2, behind: 0 },
          { name: "main", current: false, ahead: 0, behind: 2 },
          { name: "stale", current: false, ahead: 0, behind: 1 },
        ],
        commits: [
          {
            id: "aaaaaaa1111",
            short: "aaaaaaa",
            summary: "work two",
            author: "Ana",
            date: "2 hours ago",
            refs: ["HEAD -> work"],
          },
        ],
        truncated: false,
      }),
      moveMain: async (name) => {
        moved.push(name);
        main = "2222222";
        return { branch: name, main, commits: 2 };
      },
    });
    actions.openWorkbench("git");
    render(Workbench);

    const strip = screen.getByRole("navigation", { name: "Workbench tabs" });
    expect(within(strip).getByRole("button", { name: "Git" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("heading", { name: "Git" })).toBeTruthy();
    expect(await screen.findByText("2 ahead main")).toBeTruthy();
    expect(screen.getByText("work two")).toBeTruthy();

    // A diverged branch gets no move button; only the fast-forwardable one does.
    expect(screen.getAllByRole("button", { name: "Move main here" })).toHaveLength(1);
    await fireEvent.click(screen.getByRole("button", { name: "Move main here" }));
    expect(screen.getByText(/This fast-forwards main by 2 commits/)).toBeTruthy();
    expect(moved).toEqual([]);
    await fireEvent.click(screen.getByRole("button", { name: "Move main" }));
    await vi.waitFor(() => expect(moved).toEqual(["work"]));
    expect(await screen.findByText(/main now points at work/)).toBeTruthy();
  });

  it("opens the wide tabs wider and remembers each width apart", async () => {
    const original = Object.getOwnPropertyDescriptor(window, "innerWidth");
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1600 });
    try {
      actions.setWorkbenchWidth(480);
      actions.openWorkbench("diff");
      const { container } = render(Workbench);
      const aside = container.querySelector<HTMLElement>(".workbench")!;
      expect(aside.style.width).toBe("480px");

      await fireEvent.click(screen.getByRole("button", { name: "Graph" }));
      expect(aside.style.width).toBe("960px");

      await fireEvent.keyDown(screen.getByRole("button", { name: "Resize workbench" }), { key: "ArrowLeft" });
      expect(state().workbenchWideWidth).toBe(984);
      expect(aside.style.width).toBe("984px");

      await fireEvent.click(screen.getByRole("button", { name: "Diff" }));
      expect(aside.style.width).toBe("480px");
      expect(state().workbenchWidth).toBe(480);

      await fireEvent.click(screen.getByRole("button", { name: "Memory" }));
      expect(aside.style.width).toBe("984px");

      await fireEvent.doubleClick(screen.getByRole("button", { name: "Resize workbench" }));
      expect(state().workbenchWideWidth).toBeNull();
      expect(aside.style.width).toBe("960px");
    } finally {
      if (original) Object.defineProperty(window, "innerWidth", original);
    }
  });

  it("hands openTrace filters to the panel's Trace tab", async () => {
    const row: TraceRow = {
      id: 7,
      ts_ms: 0,
      model: "worker-model",
      base_url: "https://api.example.invalid/v1",
      provider: "primary",
      http_status: 200,
      usage: null,
      finish_reason: null,
      is_complete: true,
      version: 2,
      ttft_ms: 120,
      generation_ms: 120,
      reasoning_ms: null,
      connect_ms: null,
      upload_ms: null,
      server_ms: null,
      tools_ms: null,
      prep_ms: null,
      compact_ms: null,
      backoff_ms: null,
      stall_ms: null,
      chunks: null,
      error: null,
      cancelled: false,
      response_truncated: false,
      response_evicted: false,
      guard_cut: null,
      run_id: "run-1",
      attribution: {
        actor_kind: "worker",
        agent_id: "design-trace",
        session_id: "session-1",
        agent_name: "Design trace",
        worker_id: null,
        plan_id: null,
        plan_step: null,
        turn_id: null,
        purpose: "turn",
      },
      cost_micros: null,
      priced: false,
      retained: false,
    };
    seedTracePagePreview({
      list: async () => ({ rows: [row], total: 1, unpriced: 0, has_more: false, next_cursor: null, capped: false }),
      detail: async () => null,
      settings: async () => ({ log_bodies: false, ring_capacity: 128, journal_bytes: 0, history_bytes: 0 }),
      setLogBodies: async () => ({ log_bodies: false, ring_capacity: 128, journal_bytes: 0, history_bytes: 0 }),
      clearLive: async () => undefined,
      deleteHistory: async () => ({ deleted_files: 0, bytes: 0 }),
      exportCalls: async () => ({ path: "", count: 0, includes_bodies: false }),
      revealExport: async () => undefined,
    });
    actions.openTrace({ agent: "design-trace" });
    render(Workbench);

    await vi.waitFor(
      () => expect((screen.getByRole("combobox", { name: "Agent" }) as HTMLSelectElement).value).toBe("design-trace"),
      { timeout: 15000 },
    );
    expect(state().traceEntry).toBeNull();
    expect(state().workbenchTab).toBe("trace");
  });
});

afterEach(() => {
  seedFilesPreview(null);
  seedGitPreview(null);
  seedTracePagePreview(null);
  vi.unstubAllGlobals();
});
