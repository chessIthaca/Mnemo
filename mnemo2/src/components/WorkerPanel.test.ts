import { act, fireEvent, render, screen, within } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TeamTransport } from "../lib/teamEngine";
import { emptyProjectUsageView, type TeamEvent, type TeamWorkerRecord } from "../lib/teamState";

const team = vi.hoisted(() => ({
  transport: null as (TeamTransport & { emit: (event: TeamEvent) => void }) | null,
}));

vi.mock("../lib/teamEngine", () => ({
  createTeamTransport: () => team.transport,
}));
vi.mock("../lib/leadEngine", () => ({ createLeadTransport: () => null }));
vi.mock("../lib/projectEngine", () => ({
  createProjectTransport: () => ({
    attach: async () => ({ conflict_pid: null, conflict_started_at: null }),
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
vi.mock("../lib/runtime", () => ({
  isTauriRuntime: () => true,
  probeMnemo2Host: async () => true,
}));

import App from "../App.svelte";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";

const state = () => get(app);

function nativeWorker(overrides: Partial<TeamWorkerRecord> = {}): TeamWorkerRecord {
  return {
    id: "ana",
    name: "Ana",
    brief: "Ship the worker panel",
    model: "test/scripted-a",
    model_id: "scripted-a",
    reasoning_effort: "high",
    tier: "balanced",
    branch: "wt/agent-ana",
    worktree: "C:/repo/.worktrees/agent-ana",
    status: "working",
    generation: 1,
    transcript: {
      messages: [
        { role: "user", content: "Ship the worker panel" },
        { role: "assistant", content: [{ type: "text", text: "Ana here: reading the layout first." }] },
      ],
    },
    pending: null,
    used_tokens: 120,
    max_tokens: 1000,
    total_tokens: 120,
    prompt_tokens: 90,
    completion_tokens: 30,
    cached_tokens: 10,
    requests: 4,
    cost_micros: 500_000,
    created_at_ms: 1,
    elapsed_ms: 65_000,
    turns: 4,
    last_activity_ms: 65_000,
    checks: [],
    findings: [],
    review: null,
    fixer_finding_id: null,
    merge: null,
    error: null,
    ...overrides,
  };
}

function kaiWorker(overrides: Partial<TeamWorkerRecord> = {}): TeamWorkerRecord {
  return nativeWorker({
    id: "kai",
    name: "Kai",
    model: "test/scripted-b",
    model_id: "scripted-b",
    reasoning_effort: "medium",
    tier: "flash",
    branch: "wt/agent-kai",
    worktree: "C:/repo/.worktrees/agent-kai",
    transcript: {
      messages: [
        { role: "user", content: "Ship the worker panel" },
        { role: "assistant", content: [{ type: "text", text: "Kai wiring the store into the panel." }] },
      ],
    },
    ...overrides,
  });
}

/** A full transport fake over mutable records; `emit` drives the store's live stream. */
function makeTeamTransport(records: TeamWorkerRecord[]): TeamTransport & { emit: (event: TeamEvent) => void } {
  let current = records;
  let handler: (event: TeamEvent) => void = () => {};
  const transport: TeamTransport = {
    snapshot: vi.fn(async () => ({
      workers: current,
      slots: 3,
      active: current.filter((worker) => ["dispatching", "working", "fixing"].includes(worker.status)).length,
      usage: emptyProjectUsageView(),
    })),
    dispatch: vi.fn(async () => current[0]),
    message: vi.fn(async () => current[0]),
    pause: vi.fn(async () => current[0]),
    interrupt: vi.fn(async () => current[0]),
    stop: vi.fn(async () => current[0]),
    resume: vi.fn(async () => current[0]),
    setModel: vi.fn(async () => current[0]),
    compact: vi.fn(async () => current[0]),
    review: vi.fn(async () => current[0]),
    recordCheck: vi.fn(async () => current[0]),
    resolveFinding: vi.fn(async () => current[0]),
    reviewFix: vi.fn(async () => current[0]),
    routeFix: vi.fn(async () => current[0]),
    merge: vi.fn(async () => current[0]),
    revertMerge: vi.fn(async () => current[0]),
    stopAll: vi.fn(async () => {}),
    setSlots: vi.fn(async () => ({
      workers: current,
      slots: 3,
      active: 1,
      usage: emptyProjectUsageView(),
    })),
    deletePreview: vi.fn(async (ids: string[]) =>
      ids.map((id) => ({
        worker_id: id,
        name: current.find((worker) => worker.id === id)?.name ?? id,
        branch: current.find((worker) => worker.id === id)?.branch ?? `wt/agent-${id}`,
        live: false,
        checkout: true,
        uncommitted: false,
        unmerged_commits: 0,
        shared_branch: false,
        blocked: null,
        superseded_by: null,
      })),
    ),
    deleteWorkers: vi.fn(async (ids: string[]) => ({
      results: ids.map((id) => ({ worker_id: id, name: "Worker", error: null })),
      snapshot: {
        workers: current,
        slots: 3,
        active: 1,
        usage: emptyProjectUsageView(),
      },
    })),
    messages: vi.fn(async () => []),
    onEvent: vi.fn(async (listener: (event: TeamEvent) => void) => {
      handler = listener;
      return () => {};
    }),
  };
  return Object.assign(transport, { emit: (event: TeamEvent) => handler(event) });
}

/** Open "mnemo" with the given live team and wait for the snapshot to land. */
async function openAppWith(records: TeamWorkerRecord[]) {
  const transport = makeTeamTransport(records);
  team.transport = transport;
  await actions.openProject("mnemo");
  await vi.waitFor(() => expect(state().workers.length).toBe(records.length));
  return transport;
}

/** Store writes outside fireEvent need act() so Svelte flushes the DOM with them. */
function emit(transport: TeamTransport & { emit: (event: TeamEvent) => void }, event: TeamEvent) {
  return act(() => {
    (transport as { emit: (event: TeamEvent) => void }).emit(event);
  });
}

let frames: Map<number, FrameRequestCallback>;
let nextFrame: number;
let resizeCallbacks: ResizeObserverCallback[];

class ResizeObserverStub {
  disconnect = vi.fn();
  constructor(callback: ResizeObserverCallback) {
    resizeCallbacks.push(callback);
  }
  observe() {}
  unobserve() {}
}

function flushFrame() {
  const callbacks = [...frames.values()];
  frames.clear();
  for (const callback of callbacks) callback(16);
}

/** Pin the panel scroller's metrics so the follow rule can be observed. */
function panelScroller(): HTMLDivElement {
  const scroller = document.querySelector(".worker-panel .thread-messages") as HTMLDivElement | null;
  if (!scroller) throw new Error("worker panel scroller not rendered");
  return scroller;
}

function scrollMetrics(
  scroller: HTMLDivElement,
  values: { scrollHeight: number; clientHeight: number; scrollTop: number },
) {
  Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: values.scrollHeight });
  Object.defineProperty(scroller, "clientHeight", { configurable: true, value: values.clientHeight });
  scroller.scrollTop = values.scrollTop;
}

describe("WorkerPanel", () => {
  beforeEach(() => {
    app.set(fixtureState());
    frames = new Map();
    nextFrame = 1;
    resizeCallbacks = [];
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        const id = nextFrame++;
        frames.set(id, callback);
        return id;
      }),
    );
    vi.stubGlobal("cancelAnimationFrame", vi.fn((id: number) => frames.delete(id)));
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    team.transport = null;
  });

  it("opens from the lead's waiting line without leaving the lead view", async () => {
    await openAppWith([nativeWorker(), kaiWorker()]);
    const { container } = render(App);
    expect(state().route).toBe("lead");

    await fireEvent.click(screen.getByRole("button", { name: "Show Kai's output" }));

    expect(state().route).toBe("lead");
    expect(state().workerPanelId).toBe("kai");
    // The lead chat stays rendered beside the panel.
    expect(container.querySelector(".lead-workspace")).toBeTruthy();
    expect(container.querySelector(".worker-panel")).toBeTruthy();
    const select = screen.getByLabelText("Choose worker") as HTMLSelectElement;
    expect(select.value).toBe("kai");
    // The panel shows the worker thread and its saved turn.
    expect(screen.getByText("Kai wiring the store into the panel.")).toBeTruthy();
    expect(screen.getByLabelText("Reply to Kai")).toBeTruthy();
  });

  it("streams worker text deltas and tool calls into the open panel", async () => {
    const transport = await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("kai");
    render(App);
    const panel = document.querySelector(".worker-panel") as HTMLElement;
    expect(within(panel).queryByText("Streaming live from the worker.")).toBeNull();

    await emit(transport, {
      worker_id: "kai",
      generation: 1,
      event: { agent: 4, event: { kind: "text_delta", text: "Streaming live from the worker." } },
    });
    expect(screen.getByText("Streaming live from the worker.")).toBeTruthy();

    await emit(transport, {
      worker_id: "kai",
      generation: 1,
      event: { agent: 4, event: { kind: "tool_call_start", id: "call-1", name: "shell", index: 0 } },
    });
    const toolCard = panel.querySelector(".tool-card") as HTMLElement | null;
    expect(toolCard).not.toBeNull();
    expect(toolCard?.getAttribute("data-status")).toBe("running");

    // A different worker's stream does not leak into the panel.
    await emit(transport, {
      worker_id: "ana",
      generation: 1,
      event: { agent: 4, event: { kind: "text_delta", text: "Ana writes in her own thread." } },
    });
    expect(within(panel).queryByText("Ana writes in her own thread.")).toBeNull();
  });

  it("switches the shown worker from the Choose worker select", async () => {
    await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("kai");
    render(App);
    const select = screen.getByLabelText("Choose worker") as HTMLSelectElement;
    const options = Array.from(select.options).map((option) => option.value);
    expect(options).toEqual(["ana", "kai"]);

    await fireEvent.change(select, { target: { value: "ana" } });

    expect(state().workerPanelId).toBe("ana");
    expect(screen.getByText("Ana here: reading the layout first.")).toBeTruthy();
    expect(screen.getByLabelText("Worker model: scripted-a")).toBeTruthy();
    expect(screen.getByText("Working")).toBeTruthy();
    expect(screen.queryByText("Kai wiring the store into the panel.")).toBeNull();
  });

  it("keeps a finished worker selected until the user picks another", async () => {
    const transport = await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("kai");
    const { container } = render(App);
    expect(screen.getByText("Kai wiring the store into the panel.")).toBeTruthy();

    // Kai finishes: the panel keeps showing it, and the select keeps it listed.
    (transport.snapshot as ReturnType<typeof vi.fn>).mockImplementation(async () => ({
      workers: [nativeWorker(), kaiWorker({ status: "completed" })],
      slots: 3,
      active: 1,
      usage: emptyProjectUsageView(),
    }));
    await act(() => actions.syncTeam());
    expect(state().workerPanelId).toBe("kai");
    expect(container.querySelector(".worker-panel")).toBeTruthy();
    const select = screen.getByLabelText("Choose worker") as HTMLSelectElement;
    expect(Array.from(select.options).map((option) => option.value)).toEqual(["kai", "ana"]);
  });
  it("steers the worker from the panel's reply box", async () => {
    const transport = await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("kai");
    render(App);
    const box = screen.getByLabelText("Reply to Kai") as HTMLTextAreaElement;

    await fireEvent.input(box, { target: { value: "Prioritize the scroll follow rule." } });
    await fireEvent.keyDown(box, { key: "Enter" });

    expect(transport.message).toHaveBeenCalledWith("kai", "Prioritize the scroll follow rule.");
    expect(box.value).toBe("");
  });

  it("shares the right side with the Workbench: each one closes the other", async () => {
    await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("kai");
    const { container } = render(App);
    expect(container.querySelector(".worker-panel")).toBeTruthy();

    await act(() => actions.openWorkbench("diff"));
    expect(state().workerPanelId).toBeNull();
    expect(state().workbenchOpen).toBe(true);
    expect(container.querySelector(".worker-panel")).toBeNull();
    expect(container.querySelector(".workbench")).toBeTruthy();

    await act(() => actions.openWorkerPanel("kai"));
    expect(state().workbenchOpen).toBe(false);
    expect(state().workerPanelId).toBe("kai");
    expect(container.querySelector(".workbench")).toBeNull();
    expect(container.querySelector(".worker-panel")).toBeTruthy();
  });

  it("resets on project switch and when the worker vanishes from a sync", async () => {
    const transport = await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("kai");
    render(App);
    expect(state().workerPanelId).toBe("kai");

    await actions.openProject("garden-game");
    expect(state().workerPanelId).toBeNull();
    expect(state().projectId).toBe("garden-game");

    // Back to mnemo: the panel stays closed until it is asked for again.
    const transportTwo = await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("kai");
    expect(state().workerPanelId).toBe("kai");
    (transportTwo.snapshot as ReturnType<typeof vi.fn>).mockImplementation(async () => ({
      workers: [nativeWorker()],
      slots: 3,
      active: 1,
      usage: emptyProjectUsageView(),
    }));
    await act(() => actions.syncTeam());
    expect(state().workerPanelId).toBeNull();
    expect(document.querySelector(".worker-panel")).toBeNull();
  });

  it("ignores unknown workers when opening the panel", async () => {
    await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("ghost");
    expect(state().workerPanelId).toBeNull();

    actions.openWorkerPanel("ana");
    expect(state().workerPanelId).toBe("ana");
    actions.openWorkerPanel("ghost");
    expect(state().workerPanelId).toBe("ana");
  });

  it("follows the streamed thread while the reader stays near the bottom", async () => {
    const transport = await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("kai");
    const { container } = render(App);
    const scroller = panelScroller();
    scrollMetrics(scroller, { scrollHeight: 1000, clientHeight: 600, scrollTop: 400 });

    await emit(transport, {
      worker_id: "kai",
      generation: 1,
      event: { agent: 4, event: { kind: "text_delta", text: "Keeps following the tail." } },
    });
    flushFrame();
    expect(scroller.scrollTop).toBe(1000);
    expect(container.querySelector(".worker-panel")).toBeTruthy();
  });

  it("stops following once the reader scrolls up to read", async () => {
    const transport = await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("kai");
    render(App);
    const scroller = panelScroller();
    scrollMetrics(scroller, { scrollHeight: 1000, clientHeight: 600, scrollTop: 400 });

    scroller.scrollTop = 160; // 240px above the tail: the reader is reading history.
    await fireEvent.scroll(scroller);

    await emit(transport, {
      worker_id: "kai",
      generation: 1,
      event: { agent: 4, event: { kind: "text_delta", text: "New lines while reading." } },
    });
    flushFrame();
    expect(scroller.scrollTop).toBe(160);
  });

  it("hands the worker off to Mission Control and closes on demand", async () => {
    await openAppWith([nativeWorker(), kaiWorker()]);
    actions.openWorkerPanel("kai");
    render(App);
    expect(screen.getByLabelText("Worker model: scripted-b")).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Open in Mission Control" }));
    expect(state().route).toBe("team");
    expect(state().selectedWorkerId).toBe("kai");

    await act(() => actions.openLead());
    expect(state().route).toBe("lead");
    expect(screen.getByLabelText("Choose worker")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Close worker panel" }));
    expect(state().workerPanelId).toBeNull();
    expect(document.querySelector(".worker-panel")).toBeNull();
  });

  it("shows the worker's model, effort, tier, and plan label in the header", async () => {
    await openAppWith([
      nativeWorker({ plan_id: "plan-1", plan_step: 0 }),
      kaiWorker({ plan_id: "plan-1", plan_step: 1 }),
    ]);
    app.update((current) => ({
      ...current,
      board: [
        {
          id: "plan-1",
          title: "Panel polish",
          goal: "Stream workers beside the lead",
          kind: "implementation",
          status: "open",
          closed_at_ms: null,
          report_path: null,
          steps: [],
        },
      ],
    }));
    actions.openWorkerPanel("kai");
    render(App);
    const panel = document.querySelector(".worker-panel") as HTMLElement;
    expect(within(panel).getByLabelText("Worker model: scripted-b")).toBeTruthy();
    expect(within(panel).getByText("Working")).toBeTruthy();
    expect(within(panel).getByText("effort: medium")).toBeTruthy();
    expect(within(panel).getByText("tier: flash")).toBeTruthy();
    expect(within(panel).getByText("Panel polish · step 2")).toBeTruthy();
  });

  it("flags a failed worker in the lead view and opens its panel with a way out", async () => {
    const transport = await openAppWith([
      nativeWorker(),
      kaiWorker({ status: "failed", error: "cargo test failed: 2 errors" }),
    ]);
    const { container } = render(App);

    // The Needs-you line names the worker with its status, even while the lead is busy.
    expect(document.body.textContent).toContain("Needs you: Kai (Failed).");
    await fireEvent.click(screen.getByRole("button", { name: "Open Kai" }));
    expect(state().workerPanelId).toBe("kai");

    // The panel shows the failure banner: the error and the three ways out.
    const panel = container.querySelector(".worker-panel") as HTMLElement;
    expect(within(panel).getByText("cargo test failed: 2 errors")).toBeTruthy();
    expect(within(panel).getByRole("button", { name: "Continue" })).toBeTruthy();
    expect(within(panel).getByRole("button", { name: "Delete Kai" })).toBeTruthy();
    const missionButtons = within(panel).getAllByRole("button", { name: "Open in Mission Control" });
    expect(missionButtons.length).toBe(2); // header + banner
    expect(state().workerDeletion).toBeNull();

    // Delete opens the existing confirmation sheet without deleting anything.
    await fireEvent.click(within(panel).getByRole("button", { name: "Delete Kai" }));
    expect(transport.deletePreview).toHaveBeenCalledWith(["kai"]);
    expect(transport.deleteWorkers).not.toHaveBeenCalled();
    expect(await screen.findByText("Delete Kai?")).toBeTruthy();
    expect(within(panel).getByRole("button", { name: "Continue" })).toBeTruthy();
  });

  it("continues a failed worker from the banner via the existing resume action", async () => {
    const transport = await openAppWith([
      nativeWorker(),
      kaiWorker({ status: "interrupted" }),
    ]);
    actions.openWorkerPanel("kai");
    render(App);
    const panel = document.querySelector(".worker-panel") as HTMLElement;
    expect(within(panel).getByRole("button", { name: "Continue" })).toBeTruthy();

    await fireEvent.click(within(panel).getByRole("button", { name: "Continue" }));
    expect(transport.resume).toHaveBeenCalledWith("kai");
    expect(state().workerDeletion).toBeNull();
  });

  it("lists needs-you workers in the Choose worker select", async () => {
    await openAppWith([
      nativeWorker(),
      kaiWorker({ status: "failed", error: "out of budget" }),
    ]);
    actions.openWorkerPanel("ana");
    render(App);
    const select = screen.getByLabelText("Choose worker") as HTMLSelectElement;
    expect(Array.from(select.options).map((option) => option.value)).toEqual(["ana", "kai"]);
  });
});