import { act, fireEvent, render, screen, within } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import {
  createAuditWorker,
  createHighRiskWorker,
  createWorkers,
  fixtureState,
} from "../test/fixtures";
import { initialLeadRuntime } from "../lib/leadState";
import type { LeadEntry, Worker } from "../lib/types";
import LeadChat from "./LeadChat.svelte";

function setLeadEntries(entries: LeadEntry[]) {
  app.update((state) => ({ ...state, leadEntries: entries }));
}

function setRuntime(patch: Partial<ReturnType<typeof initialLeadRuntime>>) {
  app.update((state) => ({
    ...state,
    leadRuntime: {
      ...initialLeadRuntime(),
      mode: "native",
      status: "ready",
      generation: 1,
      ...patch,
    },
  }));
}

let frames: Map<number, FrameRequestCallback>;
let nextFrame: number;
let resizeCallbacks: ResizeObserverCallback[];
let resizeObservers: ResizeObserverStub[];
let cancelAnimationFrame: ReturnType<typeof vi.fn>;

class ResizeObserverStub {
  disconnect = vi.fn();
  constructor(callback: ResizeObserverCallback) {
    resizeCallbacks.push(callback);
    resizeObservers.push(this);
  }
  observe() {}
  unobserve() {}
}

function flushFrame() {
  const callbacks = [...frames.values()];
  frames.clear();
  for (const callback of callbacks) callback(16);
}

function triggerResize() {
  for (const callback of resizeCallbacks) callback([], {} as ResizeObserver);
}

function toolEntry(overrides: Partial<Extract<LeadEntry, { kind: "tool" }>> = {}): LeadEntry {
  return {
    id: "scroll-tool",
    kind: "tool",
    time: "now",
    toolCallId: "scroll-call",
    toolName: "shell",
    arguments: '{"command":"cargo test"}',
    liveOutput: "running tests",
    result: null,
    ...overrides,
  };
}

function scrollMetrics(
  container: HTMLElement,
  values: { scrollHeight: number; clientHeight: number; scrollTop: number },
) {
  const scroller = container.querySelector(".lead-scroll") as HTMLDivElement;
  Object.defineProperty(scroller, "scrollHeight", {
    configurable: true,
    value: values.scrollHeight,
  });
  Object.defineProperty(scroller, "clientHeight", {
    configurable: true,
    value: values.clientHeight,
  });
  scroller.scrollTop = values.scrollTop;
  return scroller;
}

function setScrollHeight(scroller: HTMLDivElement, value: number) {
  Object.defineProperty(scroller, "scrollHeight", { configurable: true, value });
}

describe("LeadChat", () => {
  beforeEach(() => {
    app.set(fixtureState());
    actions.openProject("mnemo");
    frames = new Map();
    nextFrame = 1;
    resizeCallbacks = [];
    resizeObservers = [];
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        const id = nextFrame++;
        frames.set(id, callback);
        return id;
      }),
    );
    cancelAnimationFrame = vi.fn((id: number) => frames.delete(id));
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrame);
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("renders no global run strip or redundant lead heading", () => {
    const { container } = render(LeadChat);
    expect(screen.queryByText(/Run 04/)).toBeNull();
    expect(container.querySelector(".lead-mark")).toBeNull();
    expect(container.querySelector(".lead-heading")).toBeNull();
  });

  it("shows the configured lead model", () => {
    render(LeadChat);
    expect(screen.getAllByText("claude-opus-5-5").length).toBeGreaterThan(0);
    expect(screen.queryByText("ATLAS CORE")).toBeNull();
  });

  it("sends a message from the composer into the lead conversation", async () => {
    render(LeadChat);
    const input = screen.getByRole("textbox", { name: "Message the lead" });
    await fireEvent.input(input, { target: { value: "Keep the review flow explicit." } });
    await fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    expect(await screen.findByText("Keep the review flow explicit.")).toBeTruthy();
    expect((input as HTMLTextAreaElement).value).toBe("");
  });

  it("shows an inline error with retry when the native lead fails", () => {
    setRuntime({ status: "error", error: "Engine refused the request." });
    const { container } = render(LeadChat);
    expect(screen.getByRole("alert").textContent).toContain("Engine refused the request.");
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
    expect(container.querySelector(".lead-stream")?.lastElementChild?.classList.contains("lead-error")).toBe(true);
  });

  it("shows automatic provider retries and terminal LLM errors at the conversation tail", async () => {
    setRuntime({ busy: true, status: "ready", error: "HTTP 429 overloaded", errorRetrying: true });
    const { container } = render(LeadChat);
    expect(screen.getByRole("alert").textContent).toContain("HTTP 429 overloaded");
    expect(screen.getByText("RETRYING")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();

    setRuntime({ busy: false, status: "error", error: "HTTP 429 overloaded", errorRetrying: false });
    expect(await screen.findByRole("button", { name: "Retry" })).toBeTruthy();
    expect(container.querySelector(".lead-stream")?.lastElementChild?.classList.contains("lead-error")).toBe(true);
  });

  it("docks an approval on the composer, outside the conversation", () => {
    setRuntime({
      pending: {
        kind: "approval",
        request_id: "req-1",
        tool_name: "write_file",
      },
    });
    const { container } = render(LeadChat);
    expect(screen.getByText("APPROVAL REQUIRED")).toBeTruthy();
    const approve = screen.getByRole("button", { name: "Approve" }) as HTMLButtonElement;
    expect(approve.disabled).toBe(false);
    expect(screen.getByRole("button", { name: "Deny" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Stop the lead" })).toBeTruthy();
    expect(container.querySelector(".lead-stream .lead-pending")).toBeNull();
    expect(container.querySelector(".composer-dock .pending-prompt")).not.toBeNull();
  });

  it("docks a question with its options on the composer and keeps the chat readable", () => {
    setRuntime({
      pending: {
        kind: "question",
        question_id: "q-1",
        question: "Which branch should I target?",
        options: [{ label: "main" }, { label: "wt/mnemo" }],
      },
    });
    const { container } = render(LeadChat);
    expect(screen.getByText("Which branch should I target?")).toBeTruthy();
    expect(container.querySelector(".composer-dock .pending-prompt .markdown")).not.toBeNull();
    expect(container.querySelector(".lead-scroll .lead-pending")).toBeNull();
    const main = screen.getByRole("button", { name: /main/ }) as HTMLButtonElement;
    expect(main.disabled).toBe(false);
    expect(screen.getByRole("button", { name: /wt\/mnemo/ })).toBeTruthy();
  });

  it("shows a free-form question and points to the composer for the answer", () => {
    setRuntime({
      pending: {
        kind: "question",
        question_id: "q-2",
        question: "What should the first worker build?",
      },
    });
    render(LeadChat);
    expect(screen.getByText("What should the first worker build?")).toBeTruthy();
    expect(screen.getByText("Type your answer below and send it.")).toBeTruthy();
    expect(screen.queryByText("The lead needs input to continue.")).toBeNull();
  });

  it("names an unanswerable pending kind instead of a bare waiting-for-you banner", () => {
    setRuntime({ pending: { kind: "policy_hook", hook_id: "hook-1" } });
    render(LeadChat);
    expect(screen.queryByText("WAITING FOR YOU")).toBeNull();
    expect(screen.queryByText("The lead needs input to continue.")).toBeNull();
    expect(screen.getByText(/paused on a policy hook/)).toBeTruthy();
  });

  it("renders human tool headlines and reveals the real details on expansion", async () => {
    vi.spyOn(Date, "now").mockReturnValue(10_000);
    const entries: LeadEntry[] = [
      {
        id: "tool-running",
        kind: "tool",
        time: "now",
        toolCallId: "call-running",
        toolName: "shell",
        arguments: '{"command":"cargo test"}',
        liveOutput: "running tests",
        result: null,
        startedAtMs: 7000,
      },
      {
        id: "tool-done",
        kind: "tool",
        time: "now",
        toolCallId: "call-done",
        toolName: "read_files",
        arguments: '{"files":["README.md"]}',
        liveOutput: "",
        result: { success: true, output: "read 42 lines" },
        startedAtMs: 1000,
        endedAtMs: 5000,
      },
      {
        id: "tool-failed",
        kind: "tool",
        time: "now",
        toolCallId: "call-failed",
        toolName: "write_file",
        arguments: '{"path":"README.md"}',
        liveOutput: "",
        result: { success: false, output: "permission denied" },
      },
    ];
    setRuntime({ busy: true });
    setLeadEntries(entries);
    render(LeadChat);

    const shellSummary = screen.getByText("Running a command").closest("summary")!;
    expect(shellSummary.textContent).not.toContain("shell");
    expect(shellSummary.textContent).toContain("0:03");
    expect(shellSummary.textContent).not.toContain("now");
    expect(screen.getByText("Reading a file").closest("summary")?.textContent).toContain("0:04");
    expect(screen.getByText("Writing files")).toBeTruthy();
    expect(screen.getByText("Running")).toBeTruthy();
    expect(screen.getByText("Completed")).toBeTruthy();
    expect(screen.getByText("Failed")).toBeTruthy();
    expect(screen.getByText("running tests")).toBeTruthy();
    expect(screen.getByText("permission denied")).toBeTruthy();
    expect(screen.queryByText("read 42 lines")).toBeNull();

    await fireEvent.click(screen.getByText("Running a command"));
    const shellCard = screen.getByLabelText("Running a command — Running");
    expect(shellCard.querySelector("details")?.open).toBe(true);
    expect(await within(shellCard).findByText("shell")).toBeTruthy();
    expect(shellCard.textContent).toContain('"command": "cargo test"');
    expect(within(shellCard).getByText("running tests")).toBeTruthy();

    await fireEvent.click(screen.getByText("Reading a file"));
    expect(await screen.findByText("read_files")).toBeTruthy();
    expect(await screen.findByText("read 42 lines")).toBeTruthy();
    await fireEvent.click(screen.getByText("Writing files"));
    expect(await screen.findByText("write_file")).toBeTruthy();
  });

  it("shows inline thinking with elapsed time and yields to the next tool", async () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(10_000);
    setRuntime({ busy: true, turnStartedAtMs: 4000 });
    setLeadEntries([
      toolEntry({
        result: { success: true, output: "done" },
        liveOutput: "",
        startedAtMs: 1000,
        endedAtMs: 5000,
      }),
    ]);
    const { container } = render(LeadChat);
    const thinking = container.querySelector(".thinking-status");
    expect(thinking?.textContent).toContain("Thinking…");
    expect(thinking?.textContent).toContain("0:06");
    expect(thinking?.previousElementSibling?.classList.contains("tool-activity-row")).toBe(true);
    expect(screen.queryByText("Working")).toBeNull();

    setLeadEntries([toolEntry()]);
    await vi.waitFor(() => expect(container.querySelector(".thinking-status")).toBeNull());
    now.mockRestore();
  });

  it("shows each batched tool's own elapsed suffix instead of cumulative queue time", () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(9000);
    setRuntime({ busy: true });
    setLeadEntries([
      toolEntry({
        id: "tool-a",
        toolCallId: "call-a",
        toolName: "read_files",
        arguments: '{"files":["README.md"]}',
        liveOutput: "",
        // Announced with the batch at 1000, executed 2000 → 6000.
        startedAtMs: 2000,
        endedAtMs: 6000,
        result: { success: true, output: "read 42 lines" },
      }),
      toolEntry({
        id: "tool-b",
        toolCallId: "call-b",
        // Announced at 1000 as well, executed from 6000; still running at 9000.
        liveOutput: "running tests",
        startedAtMs: 6000,
        result: null,
      }),
    ]);
    render(LeadChat);
    const finished = screen.getByText("Reading a file").closest("summary")!;
    expect(finished.textContent).toContain("0:04");
    expect(finished.textContent).not.toContain("0:05");
    const running = screen.getByText("Running a command").closest("summary")!;
    expect(running.textContent).toContain("0:03");
    expect(running.textContent).not.toContain("0:08");
    now.mockRestore();
  });

  it("does not let an unfinished tool from an older turn suppress thinking", () => {
    setRuntime({ busy: true, turnStartedAtMs: 4000 });
    setLeadEntries([toolEntry({ startedAtMs: 1000, result: null })]);
    const { container } = render(LeadChat);
    expect(container.querySelector(".thinking-status")).not.toBeNull();
  });

  it("hides empty lead messages and spaces a tool run from the next message", () => {
    setLeadEntries([
      { id: "m1", kind: "message", author: "lead", time: "now", body: "Let me check." },
      { id: "m2", kind: "message", author: "lead", time: "now", body: "  " },
      toolEntry({ id: "t1", result: { success: true, output: "ok" } as never }),
      { id: "m3", kind: "message", author: "lead", time: "now", body: "Done." },
    ] as LeadEntry[]);
    const { container } = render(LeadChat);
    const messages = container.querySelectorAll(".chat-entry");
    expect(messages).toHaveLength(2);
    expect(messages[0].classList.contains("before-tool")).toBe(true);
    expect(container.querySelector(".tool-activity-row")?.classList.contains("before-message")).toBe(true);
  });

  it("connects only consecutive tool entries with an activity-run line", () => {
    setLeadEntries([
      toolEntry({ id: "tool-one", toolCallId: "one" }),
      toolEntry({ id: "tool-two", toolCallId: "two" }),
      { id: "between", kind: "message", time: "now", author: "lead", body: "Between tools" },
      toolEntry({ id: "tool-three", toolCallId: "three" }),
    ]);
    const { container } = render(LeadChat);
    const rows = container.querySelectorAll(".tool-activity-row");
    expect(rows).toHaveLength(3);
    expect(rows[0].classList.contains("tool-connected")).toBe(true);
    expect(rows[1].classList.contains("tool-connected")).toBe(false);
    expect(rows[2].classList.contains("tool-connected")).toBe(false);
  });

  it("does not open a result disclosure when the output is empty", () => {
    const entries: LeadEntry[] = [
      {
        id: "empty-success",
        kind: "tool",
        time: "now",
        toolCallId: "empty-success",
        toolName: "noop",
        arguments: "",
        liveOutput: "",
        result: { success: true, output: "" },
      },
      {
        id: "empty-failure",
        kind: "tool",
        time: "now",
        toolCallId: "empty-failure",
        toolName: "guard",
        arguments: "",
        liveOutput: "",
        result: { success: false, output: "" },
      },
    ];
    setRuntime({ busy: false });
    setLeadEntries(entries);
    render(LeadChat);

    const completed = screen.getByLabelText("Using Noop — Completed");
    expect(within(completed).getByText("Completed")).toBeTruthy();
    expect(within(completed).queryByText("Result")).toBeNull();

    const failed = screen.getByLabelText("Using Guard — Failed");
    expect(within(failed).getByText("Failed")).toBeTruthy();
    expect(within(failed).queryByText("Error")).toBeNull();
  });

  it("shows a host tool as team work in progress, not as a user decision", () => {
    setRuntime({
      busy: true,
      pending: {
        kind: "host_tool_call",
        request_id: "host-1",
        tool_call_id: "host-1",
        tool_name: "spawn_agent",
        args: { task: "review the diff" },
      },
    });
    setLeadEntries([
      {
        id: "tool-host",
        kind: "tool",
        time: "now",
        toolCallId: "host-1",
        toolName: "spawn_agent",
        arguments: '{"task":"review the diff"}',
        liveOutput: "",
        result: null,
      },
    ]);
    render(LeadChat);

    expect(screen.getByText(/team runtime is executing/i)).toBeTruthy();
    expect(screen.queryByText("WAITING FOR YOU")).toBeNull();
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
  });

  it("shows team updates and reviews as a headline that expands to Markdown", async () => {
    setLeadEntries([
      {
        id: "team-markdown",
        kind: "team-message",
        time: "now",
        workerId: "ana",
        body: "Worker **update**",
      },
      {
        id: "review-markdown",
        kind: "review",
        time: "now",
        workerId: "ana",
        title: "Review complete",
        body: "Finding `F-1` is **closed**.",
      },
    ]);
    const { container } = render(LeadChat);
    flushFrame();
    await act();

    const rows = [...container.querySelectorAll<HTMLDetailsElement>("details.event-row")];
    expect(rows).toHaveLength(2);
    expect(rows[1].querySelector("summary")?.textContent).toContain("Review complete");
    expect(screen.queryByText("update")).toBeNull();
    for (const row of rows) {
      row.open = true;
      row.dispatchEvent(new Event("toggle"));
    }
    await act();

    expect((await screen.findByText("update")).tagName).toBe("STRONG");
    expect((await screen.findByText("closed")).tagName).toBe("STRONG");
    expect(screen.getByText("F-1").tagName).toBe("CODE");
  });

  it("labels a high-risk lead review with its audit linkage", async () => {
    // Drain the pending project open from `beforeEach`: it resets the team.
    await act();
    const mara = createHighRiskWorker();
    app.update((state) => ({
      ...state,
      workers: [mara, createAuditWorker()],
      selectedWorkerId: "mara",
      leadEntries: [
        {
          id: "review-mara",
          kind: "review" as const,
          time: "now",
          workerId: "mara",
          title: "Lead review recorded",
          body: "Adjudicated the audit at the reviewed tip.",
        },
      ],
    }));
    render(LeadChat);
    flushFrame();
    await act();

    // The detailed evidence lives inside the collapsible review event.
    const maraSummary = (await screen.findByText("Lead review recorded"))
      .closest("summary");
    expect(maraSummary).not.toBeNull();
    await fireEvent.click(maraSummary!);

    expect(await screen.findByText("High risk")).toBeTruthy();
    expect(screen.getByText(/Independent audit linked at the reviewed tip/)).toBeTruthy();
  });

  it("marks a legacy lead review as missing structured evidence", async () => {
    // Drain the pending project open from `beforeEach`: it resets the team.
    await act();
    const source = createWorkers().find((item) => item.id === "ana")!;
    const legacyAna: Worker = {
      ...source,
      review: { summary: "legacy review", tip: "tip-ana", reviewedAtMs: 1 },
    };
    app.update((state) => ({
      ...state,
      workers: [legacyAna],
      selectedWorkerId: "ana",
      leadEntries: [
        {
          id: "review-legacy",
          kind: "review" as const,
          time: "now",
          workerId: "ana",
          title: "Lead review recorded",
          body: "Legacy tip reviewed.",
        },
      ],
    }));
    expect(legacyAna.review).not.toHaveProperty("evidence");

    render(LeadChat);
    flushFrame();
    await act();

    // Legacy evidence is expanded detail, not part of the collapsed headline.
    const legacySummary = (await screen.findByText("Lead review recorded"))
      .closest("summary");
    expect(legacySummary).not.toBeNull();
    await fireEvent.click(legacySummary!);

    expect(await screen.findByText(/Legacy review · structured evidence is missing/)).toBeTruthy();
  });

  it("says which workers an idle lead is waiting on", () => {
    setRuntime({ busy: false });
    app.update((state) => ({
      ...state,
      workers: fixtureState().workers.slice(0, 2).map((worker, index) => ({
        ...worker,
        status: index === 0 ? "working" : "merged",
      })),
    }));
    const name = fixtureState().workers[0].name;
    render(LeadChat);
    expect(document.body.textContent).toContain(`Waiting on ${name}. The lead picks up when it finishes.`);
    setRuntime({ busy: true });
  });

  it("shows no waiting line while the lead is working", () => {
    setRuntime({ busy: true });
    app.update((state) => ({
      ...state,
      workers: fixtureState().workers.map((worker) => ({ ...worker, status: "working" })),
    }));
    render(LeadChat);
    expect(screen.queryByText(/Waiting on/)).toBeNull();
  });

  it("uses the available lead width instead of a centered fixed column", () => {
    const { container } = render(LeadChat);
    const content = container.querySelector(".lead-content");
    expect(content).not.toBeNull();
    const style = getComputedStyle(content!);
    expect(style.width).toBe("100%");
    expect(style.marginLeft).toBe("0px");
    expect(style.marginRight).toBe("0px");
  });

  it("keeps human messages inline in exchange order and caps prose at 100 characters", () => {
    setLeadEntries([
      { id: "human", kind: "message", time: "now", author: "user", body: "Review this change" },
      toolEntry(),
      { id: "answer", kind: "message", time: "now", author: "lead", body: "I will inspect it." },
    ]);
    const { container } = render(LeadChat);
    const user = container.querySelector(".user-entry")!;
    const tool = container.querySelector(".tool-activity-row")!;
    const assistant = container.querySelector(".chat-entry:not(.user-entry)")!;

    expect(user.compareDocumentPosition(tool) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(tool.compareDocumentPosition(assistant) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(getComputedStyle(user).marginLeft).toBe("0px");
    expect(getComputedStyle(user).maxWidth).toBe("800px");
    const prose = assistant.querySelector(".markdown p")!;
    const proseStyle = getComputedStyle(prose);
    expect(proseStyle.fontSize).toBe("14px");
    expect(proseStyle.maxWidth).toBe(`${parseFloat(proseStyle.fontSize) * 50}px`);
  });

  it("does not follow new activity after the user scrolls up", async () => {
    setRuntime({ busy: true });
    setLeadEntries([toolEntry()]);
    const { container } = render(LeadChat);
    const scroller = scrollMetrics(container, {
      scrollHeight: 1000,
      clientHeight: 400,
      scrollTop: 600,
    });
    await fireEvent.scroll(scroller);
    flushFrame();
    await act();
    expect(scroller.scrollTop).toBe(1000);

    scroller.scrollTop = 100;
    await fireEvent.scroll(scroller);
    await act(() => {
      setLeadEntries([
        toolEntry(),
        toolEntry({ id: "new-tool", toolCallId: "new-call", liveOutput: "more" }),
      ]);
    });
    flushFrame();
    await act();
    expect(scroller.scrollTop).toBe(100);
  });

  it("follows streaming growth while the user remains near the bottom", async () => {
    setRuntime({ busy: true });
    setLeadEntries([toolEntry({ liveOutput: "aaaa" })]);
    const { container } = render(LeadChat);
    const scroller = scrollMetrics(container, {
      scrollHeight: 1000,
      clientHeight: 400,
      scrollTop: 600,
    });
    await fireEvent.scroll(scroller);
    flushFrame();
    await act();

    scroller.scrollTop = 580;
    await fireEvent.scroll(scroller);
    await act(() => {
      setLeadEntries([toolEntry({ liveOutput: "bbbb" })]);
    });
    setScrollHeight(scroller, 1200);
    await act(() => {
      triggerResize();
    });
    flushFrame();
    await act();
    expect(scroller.scrollTop).toBe(1200);
  });

  it("follows a content resize caused by asynchronous rendering when sticky", async () => {
    setRuntime({ busy: true });
    setLeadEntries([toolEntry()]);
    const { container } = render(LeadChat);
    const scroller = scrollMetrics(container, {
      scrollHeight: 1000,
      clientHeight: 400,
      scrollTop: 600,
    });
    await fireEvent.scroll(scroller);
    flushFrame();
    await act();

    setScrollHeight(scroller, 1500);
    await act(() => {
      triggerResize();
    });
    flushFrame();
    await act();
    expect(scroller.scrollTop).toBe(1500);
  });

  it("disconnects the resize observer and cancels a pending frame on unmount", async () => {
    const { unmount } = render(LeadChat);
    const observer = resizeObservers[0];
    expect(observer).toBeDefined();
    expect(frames.size).toBeGreaterThan(0);
    if (!observer) throw new Error("LeadChat did not create a ResizeObserver");

    await act(() => {
      unmount();
    });

    expect(observer.disconnect).toHaveBeenCalled();
    expect(cancelAnimationFrame).toHaveBeenCalled();
  });
});
