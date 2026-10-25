import { act, render } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Markdown from "./Markdown.svelte";

const markdown = vi.hoisted(() => {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const renderMarkdown = vi.fn((source: string) => `<p>${source}</p>`);
  return { gate, release, renderMarkdown };
});

vi.mock("../lib/markdown", async () => {
  await markdown.gate;
  return { renderMarkdown: markdown.renderMarkdown };
});

let frames: Map<number, FrameRequestCallback>;
let nextFrame: number;
let cancelAnimationFrame: ReturnType<typeof vi.fn>;

function flushFrame() {
  const callbacks = [...frames.values()];
  frames.clear();
  for (const callback of callbacks) callback(16);
}

async function settle() {
  await act();
  await Promise.resolve();
  await act();
}

describe("Markdown", () => {
  beforeEach(() => {
    markdown.renderMarkdown.mockReset();
    markdown.renderMarkdown.mockImplementation((source: string) => `<p>${source}</p>`);
    frames = new Map();
    nextFrame = 1;
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        const id = nextFrame++;
        frames.set(id, callback);
        return id;
      }),
    );
    cancelAnimationFrame = vi.fn((id: number) => {
      frames.delete(id);
    });
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrame);
  });

  afterEach(() => {
    markdown.release();
    vi.unstubAllGlobals();
  });

  it("keeps the previous frame and ignores the stale source until the latest renders", async () => {
    const { container, rerender } = render(Markdown, { source: "First" });
    expect(container.textContent).toContain("First");

    flushFrame();
    await rerender({ source: "Second" });
    flushFrame();
    expect(container.textContent).toContain("First");

    markdown.release();
    await vi.waitFor(() => {
      expect(container.textContent).toContain("Second");
    });
    await settle();

    expect(container.textContent).toContain("Second");
    expect(container.textContent).not.toContain("First");
    expect(markdown.renderMarkdown).toHaveBeenCalledTimes(1);
    expect(markdown.renderMarkdown).toHaveBeenLastCalledWith("Second");
  });

  it("coalesces several source changes into one render of the newest source", async () => {
    const { container, rerender } = render(Markdown, { source: "one" });
    flushFrame();
    await settle();
    expect(markdown.renderMarkdown).toHaveBeenLastCalledWith("one");
    markdown.renderMarkdown.mockClear();

    await rerender({ source: "two" });
    await rerender({ source: "three" });
    await rerender({ source: "four" });
    expect(markdown.renderMarkdown).not.toHaveBeenCalled();

    flushFrame();
    await settle();

    expect(markdown.renderMarkdown).toHaveBeenCalledTimes(1);
    expect(markdown.renderMarkdown).toHaveBeenLastCalledWith("four");
    expect(container.textContent).toContain("four");
  });

  it("cancels a pending frame when the component unmounts", async () => {
    const { rerender, unmount } = render(Markdown, { source: "ready" });
    flushFrame();
    await settle();
    markdown.renderMarkdown.mockClear();

    await rerender({ source: "pending" });
    await act(() => {
      unmount();
    });

    expect(cancelAnimationFrame).toHaveBeenCalled();
    flushFrame();
    await settle();
    expect(markdown.renderMarkdown).not.toHaveBeenCalled();
  });
});
