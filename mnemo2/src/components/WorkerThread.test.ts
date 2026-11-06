import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import type { Worker, WorkerThreadEntry } from "../lib/types";
import { createWorkers, fixtureState } from "../test/fixtures";
import WorkerThread from "./WorkerThread.svelte";

/** The full 429 the user reported losing — wrapped and selectable, never truncated. */
const failure =
  "provider error: Ollama returned 429 — you have reached your session usage limit, wait before retrying";

function worker(overrides: Partial<Worker> = {}): Worker {
  return { ...createWorkers()[0], status: "failed", error: failure, ...overrides };
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

/** Run every frame the thread requested, oldest first. */
function flushFrame() {
  const callbacks = [...frames.values()];
  frames.clear();
  for (const callback of callbacks) callback(16);
}

/** Pin the thread scroller's metrics so the follow rule can be observed. */
function threadScroller(container: HTMLElement): HTMLDivElement {
  const scroller = container.querySelector(".thread-messages") as HTMLDivElement | null;
  if (!scroller) throw new Error("worker thread scroller not rendered");
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

/** A worker message entry the follow tests can append. */
function message(id: string): WorkerThreadEntry {
  return { id, from: "worker", body: `Line ${id} while the thread grows.`, time: "now" };
}

describe("WorkerThread", () => {
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
  });

  it("shows the failure block with the full error text when the worker failed", () => {
    const { container } = render(WorkerThread, { worker: worker() });


    const block = container.querySelector<HTMLElement>(".thread-failure");
    if (!block) throw new Error("failure block missing");
    expect(block.getAttribute("role")).toBe("alert");
    expect(block.textContent).toContain(failure);
    expect(block.textContent).toContain("The lead can resume this worker on another model.");

    const text = block.querySelector<HTMLElement>("p");
    if (!text) throw new Error("failure text missing");
    const style = getComputedStyle(text);
    // Danger colour, wrapping, no truncation — the full text stays selectable.
    expect(style.color).toBe("var(--danger)");
    expect(style.whiteSpace).toBe("pre-wrap");
  });

  it("keeps the failure block out of the compact panel, which has its own banner", () => {
    const { container: full } = render(WorkerThread, { worker: worker() });
    const { container: compact } = render(WorkerThread, { worker: worker(), compact: true });

    expect(full.querySelector(".thread-failure")).not.toBeNull();
    expect(compact.querySelector(".thread-failure")).toBeNull();
    // The compact thread itself still renders — only the duplicate block goes.
    expect(compact.querySelector(".thread-message")).not.toBeNull();
  });

  it("shows no failure block while the worker works, nor for a completed one", () => {
    const { container: working } = render(WorkerThread, {
      worker: worker({ status: "working", error: null }),
    });
    // A completed worker with a stale error text still reads as success.
    const { container: done } = render(WorkerThread, { worker: worker({ status: "completed" }) });

    expect(working.querySelector(".thread-failure")).toBeNull();
    expect(done.querySelector(".thread-failure")).toBeNull();
  });

  it("renders a live final error in danger and a retrying one as a muted note", () => {
    const entries: WorkerThreadEntry[] = [
      { id: "note", kind: "error", time: "now", body: "429 usage limit", retrying: true },
      { id: "final", kind: "error", time: "now", body: "429 retries exhausted", retrying: false },
    ];
    const { container } = render(WorkerThread, {
      worker: worker({ status: "working", error: null, messages: entries }),
    });

    const note = container.querySelector<HTMLElement>(".thread-error-note");
    if (!note) throw new Error("retry note missing");
    expect(note.getAttribute("role")).toBe("status");
    expect(note.textContent).toBe("Provider error, retrying: 429 usage limit");
    expect(getComputedStyle(note).color).toBe("var(--muted)");

    const danger = container.querySelector<HTMLElement>(".thread-error");
    if (!danger) throw new Error("final error entry missing");
    expect(danger.getAttribute("role")).toBe("alert");
    expect(danger.textContent).toContain("WORKER ERROR");
    expect(danger.textContent).toContain("429 retries exhausted");
    expect(getComputedStyle(danger.querySelector<HTMLElement>("p")!).color).toBe("var(--danger)");
  });

  it("shows one running clock across a turn's tool cards", () => {
    const call = (id: string, startedAtMs: number) => ({
      id,
      kind: "tool" as const,
      time: "now",
      toolCallId: `call-${id}`,
      toolName: "read_files",
      arguments: "{}",
      liveOutput: "",
      result: { success: true, output: "read 1 line" },
      startedAtMs,
      endedAtMs: startedAtMs + 200,
    });
    const entries: WorkerThreadEntry[] = [call("tool-a", 1000), call("tool-b", 11_000)];
    const { container } = render(WorkerThread, {
      worker: worker({ status: "working", error: null, messages: entries }),
    });

    // Ten seconds apart, each under a second: the cards read the turn's clock, not their own.
    const statuses = [...container.querySelectorAll<HTMLElement>(".tool-status")].map((node) =>
      node.textContent?.trim(),
    );
    expect(statuses).toEqual(["0:00", "0:10"]);
  });

  it("uses the same box as the lead", () => {
    const { container } = render(WorkerThread, { worker: worker({ id: "same-box" }) });

    // One component, one look: the lead's composer shell renders here too.
    expect(container.querySelector(".composer-input")).toBeTruthy();
  });

  it("steers a working agent and clears after acceptance", async () => {
    const reply = vi.spyOn(actions, "replyWorker").mockResolvedValue(true);
    render(WorkerThread, { worker: worker({ id: "steer-1", name: "Kai", status: "working" }) });
    const box = screen.getByRole("textbox", { name: "Reply to Kai" }) as HTMLTextAreaElement;

    expect(box.placeholder).toBe("Steer Kai…");
    expect(screen.getByRole("button", { name: "Steer Kai" })).toBeTruthy();

    await fireEvent.input(box, { target: { value: "Skip the flaky test." } });
    await fireEvent.keyDown(box, { key: "Enter" });

    expect(reply).toHaveBeenCalledWith("steer-1", "Skip the flaky test.");
    // Cleared only once the backend accepted it.
    await waitFor(() => expect(box.value).toBe(""));
  });

  it("keeps the text when the message is refused", async () => {
    const reply = vi.spyOn(actions, "replyWorker").mockResolvedValue(false);
    render(WorkerThread, { worker: worker({ id: "refused-1", name: "Kai", status: "working" }) });
    const box = screen.getByRole("textbox", { name: "Reply to Kai" }) as HTMLTextAreaElement;

    await fireEvent.input(box, { target: { value: "Still here?" } });
    await fireEvent.keyDown(box, { key: "Enter" });

    await waitFor(() => expect(reply).toHaveBeenCalled());
    expect(box.value).toBe("Still here?");
  });

  it("is disabled and says so once the agent is done", async () => {
    const reply = vi.spyOn(actions, "replyWorker").mockResolvedValue(true);
    const merged = render(WorkerThread, { worker: worker({ id: "done-1", status: "merged" }) });
    const mergedBox = screen.getByRole("textbox", { name: "Reply to Ana" }) as HTMLTextAreaElement;

    expect(mergedBox.disabled).toBe(true);
    expect(mergedBox.placeholder).toBe("Ana is done");
    expect(
      (screen.getByRole("button", { name: "Send message to Ana" }) as HTMLButtonElement).disabled,
    ).toBe(true);
    await fireEvent.keyDown(mergedBox, { key: "Enter" });
    expect(reply).not.toHaveBeenCalled();
    merged.unmount();

    render(WorkerThread, {
      worker: worker({ id: "done-2", kind: "audit", status: "completed" }),
    });
    const auditBox = screen.getByRole("textbox", { name: "Reply to Ana" }) as HTMLTextAreaElement;
    expect(auditBox.disabled).toBe(true);
    expect(auditBox.placeholder).toBe("Ana finished its audit");
  });

  it("points at Resume for a stopped agent", () => {
    render(WorkerThread, { worker: worker({ id: "stopped-1", status: "stopped" }) });

    const box = screen.getByRole("textbox", { name: "Reply to Ana" }) as HTMLTextAreaElement;
    expect(box.disabled).toBe(true);
    expect(box.placeholder).toBe("Resume Ana to message it");
  });

  it("is disabled and says so for a completed agent", async () => {
    const reply = vi.spyOn(actions, "replyWorker").mockResolvedValue(true);
    render(WorkerThread, { worker: worker({ id: "open-1", status: "completed" }) });

    const box = screen.getByRole("textbox", { name: "Reply to Ana" }) as HTMLTextAreaElement;
    expect(box.disabled).toBe(true);
    expect(box.placeholder).toBe("Ana is done");
    expect(
      (screen.getByRole("button", { name: "Send message to Ana" }) as HTMLButtonElement).disabled,
    ).toBe(true);
    await fireEvent.keyDown(box, { key: "Enter" });
    expect(reply).not.toHaveBeenCalled();
  });

  it("is disabled for an in-review agent", () => {
    render(WorkerThread, { worker: worker({ id: "open-2", status: "in_review" }) });

    const box = screen.getByRole("textbox", { name: "Reply to Ana" }) as HTMLTextAreaElement;
    expect(box.disabled).toBe(true);
    expect(box.placeholder).toBe("Ana is done");
  });

  it("stays open for a failed agent", () => {
    render(WorkerThread, { worker: worker({ id: "open-3", status: "failed" }) });

    const box = screen.getByRole("textbox", { name: "Reply to Ana" }) as HTMLTextAreaElement;
    expect(box.disabled).toBe(false);
    expect(box.placeholder).toBe("Message Ana");
  });


  it("keeps a draft across unmount", async () => {
    const props = { worker: worker({ id: "draft-1", status: "working" }) };
    const first = render(WorkerThread, props);
    await fireEvent.input(screen.getByRole("textbox", { name: "Reply to Ana" }), {
      target: { value: "Half-typed thought" },
    });
    first.unmount();

    render(WorkerThread, props);
    const box = screen.getByRole("textbox", { name: "Reply to Ana" }) as HTMLTextAreaElement;
    await waitFor(() => expect(box.value).toBe("Half-typed thought"));
  });

  it("offers Pause while working and not while idle", async () => {
    const pause = vi.spyOn(actions, "pauseWorker").mockReturnValue(undefined);
    const working = render(WorkerThread, {
      worker: worker({ id: "pause-1", name: "Kai", status: "working" }),
    });

    await fireEvent.click(screen.getByRole("button", { name: "Pause Kai" }));
    expect(pause).toHaveBeenCalledWith("pause-1");
    working.unmount();

    render(WorkerThread, { worker: worker({ id: "pause-2", name: "Kai", status: "idle" }) });
    expect(screen.queryByRole("button", { name: "Pause Kai" })).toBeNull();
  });

  it("mission control follows new messages at the bottom", async () => {
    const base = worker({ id: "follow-1", status: "working", error: null });
    const { container, rerender } = render(WorkerThread, { worker: base });
    const scroller = threadScroller(container);
    scrollMetrics(scroller, { scrollHeight: 1000, clientHeight: 600, scrollTop: 400 });

    await rerender({ worker: { ...base, messages: [...base.messages, message("follow-new")] } });
    flushFrame();

    expect(scroller.scrollTop).toBe(1000);
  });

  it("mission control leaves a reader who scrolled up alone", async () => {
    const base = worker({ id: "follow-2", status: "working", error: null });
    const { container, rerender } = render(WorkerThread, { worker: base });
    const scroller = threadScroller(container);
    scrollMetrics(scroller, { scrollHeight: 1000, clientHeight: 600, scrollTop: 400 });

    scroller.scrollTop = 160; // 240 px above the tail: the reader is reading history.
    await fireEvent.scroll(scroller);

    await rerender({ worker: { ...base, messages: [...base.messages, message("follow-later")] } });
    flushFrame();

    expect(scroller.scrollTop).toBe(160);
  });

  it("keeps the message box outside the scrolling messages", () => {
    const full = render(WorkerThread, { worker: worker({ id: "pin-1", status: "working" }) });
    const compact = render(WorkerThread, {
      worker: worker({ id: "pin-2", status: "working" }),
      compact: true,
    });

    for (const { container } of [full, compact]) {
      const messages = container.querySelector(".thread-messages");
      if (!messages) throw new Error("thread scroller missing");
      expect(messages.querySelector(".thread-composer")).toBeNull();
      expect(container.querySelector(".thread-composer")).not.toBeNull();
    }
  });
});
