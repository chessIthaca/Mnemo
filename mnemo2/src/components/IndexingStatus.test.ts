import { render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import { newerIndexing, type IndexingSnapshot } from "../lib/indexingEngine";
import FirstIndexOverlay from "./FirstIndexOverlay.svelte";
import IndexingStatus from "./IndexingStatus.svelte";

function snapshot(patch: Partial<IndexingSnapshot>): IndexingSnapshot {
  return { revision: 1, firstRun: false, code: null, memory: null, ...patch };
}

function setIndexing(indexing: IndexingSnapshot | null) {
  app.update((state) => ({ ...state, indexing }));
}

describe("indexing status", () => {
  beforeEach(() => app.set(fixtureState()));

  it("shows each running pass with its count and a progress bar", () => {
    setIndexing(snapshot({
      code: { state: "running", done: 120, total: 450, detail: null },
      memory: { state: "running", done: 0, total: 0, detail: null },
    }));
    render(IndexingStatus);
    expect(screen.getByText("Indexing code · 120/450 files")).toBeTruthy();
    expect(screen.getByText("Updating memory")).toBeTruthy();
    const bar = screen.getByRole("progressbar", { name: "Code index progress" });
    expect(bar.getAttribute("aria-valuenow")).toBe("27");
  });

  it("shows nothing once the passes are done and names a failure", () => {
    setIndexing(snapshot({
      code: { state: "done", done: 450, total: 450, detail: "450 files indexed" },
      memory: { state: "failed", done: 0, total: 3, detail: "disk full" },
    }));
    render(IndexingStatus);
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.getByText("Memory update failed").getAttribute("title")).toBe("disk full");
  });

  it("confirms a pass that finishes while open, then hides it", async () => {
    vi.useFakeTimers();
    try {
      setIndexing(snapshot({ revision: 1, code: { state: "running", done: 0, total: 0, detail: null } }));
      render(IndexingStatus);
      expect(screen.getByText("Indexing code")).toBeTruthy();
      setIndexing(snapshot({ revision: 2, code: { state: "done", done: 9, total: 9, detail: "9 files indexed" } }));
      await tick();
      expect(screen.getByText("Code index up to date").getAttribute("title")).toBe("9 files indexed");
      vi.advanceTimersByTime(4100);
      await tick();
      expect(screen.queryByText("Code index up to date")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not announce a pass that was already done", () => {
    setIndexing(snapshot({ code: { state: "done", done: 9, total: 9, detail: "9 files indexed" } }));
    render(IndexingStatus);
    expect(screen.queryByText("Code index up to date")).toBeNull();
  });

  it("shows the Laya check with its task count and a progress bar", () => {
    setIndexing(snapshot({ laya: { state: "running", done: 34, total: 207, detail: null } }));
    render(IndexingStatus);
    expect(screen.getByText("Checking Laya · 34/207 tasks")).toBeTruthy();
    const bar = screen.getByRole("progressbar", { name: "Laya check progress" });
    expect(bar.getAttribute("aria-valuenow")).toBe("16");
  });

  it("shows the Laya check before its total is known", () => {
    setIndexing(snapshot({ laya: { state: "running", done: 0, total: 0, detail: null } }));
    render(IndexingStatus);
    expect(screen.getByText("Checking Laya")).toBeTruthy();
  });

  it("confirms the Laya check when it finishes", async () => {
    vi.useFakeTimers();
    try {
      setIndexing(snapshot({ revision: 1, laya: { state: "running", done: 0, total: 207, detail: null } }));
      render(IndexingStatus);
      expect(screen.getByText("Checking Laya · 0/207 tasks")).toBeTruthy();
      setIndexing(snapshot({ revision: 2, laya: { state: "done", done: 207, total: 207, detail: "gates applied" } }));
      await tick();
      expect(screen.getByText("Laya checked").getAttribute("title")).toBe("gates applied");
    } finally {
      vi.useRealTimers();
    }
  });

  it("names a failed Laya check", () => {
    setIndexing(snapshot({ laya: { state: "failed", done: 0, total: 207, detail: "Laya stopped answering" } }));
    render(IndexingStatus);
    expect(screen.getByText("Laya check failed").getAttribute("title")).toBe("Laya stopped answering");
  });

  it("holds the project only during a first run's code index", () => {
    setIndexing(snapshot({ firstRun: true, code: { state: "running", done: 0, total: 0, detail: null } }));
    const { unmount } = render(FirstIndexOverlay);
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(screen.getByText("Counting files…")).toBeTruthy();
    unmount();

    setIndexing(snapshot({ firstRun: false, code: { state: "running", done: 3, total: 9, detail: null } }));
    render(FirstIndexOverlay);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("never lets a late snapshot replace a newer one", () => {
    const done = snapshot({ revision: 4, firstRun: true, code: { state: "done", done: 9, total: 9, detail: null } });
    const stale = snapshot({ revision: 3, firstRun: true, code: { state: "running", done: 8, total: 9, detail: null } });
    expect(newerIndexing(done, stale)).toBe(done);
    expect(newerIndexing(stale, done)).toBe(done);
    expect(newerIndexing(null, stale)).toBe(stale);
  });
});
