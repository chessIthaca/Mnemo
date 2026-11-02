import { render } from "@testing-library/svelte";
import { beforeEach, describe, expect, it } from "vitest";
import { app } from "../lib/store";
import type { Worker, WorkerThreadEntry } from "../lib/types";
import { createWorkers, fixtureState } from "../test/fixtures";
import WorkerThread from "./WorkerThread.svelte";

/** The full 429 the user reported losing — wrapped and selectable, never truncated. */
const failure =
  "provider error: Ollama returned 429 — you have reached your session usage limit, wait before retrying";

function worker(overrides: Partial<Worker> = {}): Worker {
  return { ...createWorkers()[0], status: "failed", error: failure, ...overrides };
}

describe("WorkerThread", () => {
  beforeEach(() => app.set(fixtureState()));

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
});