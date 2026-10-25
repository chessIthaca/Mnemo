import { act, render } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Markdown from "./Markdown.svelte";

const markdown = vi.hoisted(() => ({
  attempts: 0,
  renderMarkdown: vi.fn((source: string) => `<p>${source}</p>`),
}));

vi.mock("../lib/markdown", () => {
  markdown.attempts += 1;
  if (markdown.attempts === 1) {
    throw new Error("markdown chunk failed");
  }
  return { renderMarkdown: markdown.renderMarkdown };
});

let frames: Map<number, FrameRequestCallback>;
let nextFrame: number;

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

describe("Markdown retry", () => {
  beforeEach(() => {
    markdown.attempts = 0;
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
    vi.stubGlobal("cancelAnimationFrame", vi.fn((id: number) => frames.delete(id)));
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("retries a rejected renderer import on a later render", async () => {
    const { container, rerender } = render(Markdown, { source: "First" });
    flushFrame();
    await vi.waitFor(() => {
      expect(markdown.attempts).toBe(1);
    });
    await settle();
    expect(container.textContent).toContain("First");
    expect(markdown.renderMarkdown).not.toHaveBeenCalled();

    vi.resetModules();
    await rerender({ source: "Second" });
    flushFrame();
    await vi.waitFor(() => {
      expect(container.textContent).toContain("Second");
    });
    expect(markdown.attempts).toBe(2);
    expect(markdown.renderMarkdown).toHaveBeenCalledWith("Second");
  });
});
