import { render } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import Markdown from "./Markdown.svelte";

const markdown = vi.hoisted(() => ({
  renderMarkdown: vi.fn((source: string) => `<p>${source}</p>`),
}));

vi.mock("../lib/markdown", () => ({
  renderMarkdown: markdown.renderMarkdown,
}));

describe("Markdown resilience", () => {
  beforeEach(() => {
    markdown.renderMarkdown.mockReset();
    markdown.renderMarkdown.mockImplementation((source: string) => `<p>${source}</p>`);
    vi.unstubAllGlobals();
  });

  it("renders formatted HTML instead of a plain-text loading frame", () => {
    markdown.renderMarkdown.mockReturnValue("<h2>Formatted</h2>");
    const { container } = render(Markdown, { source: "raw" });

    expect(container.querySelector("h2")?.textContent).toBe("Formatted");
    expect(container.querySelector(".markdown-loading")).toBeNull();
  });

  it("does not defer text updates through animation frames or dynamic imports", async () => {
    const frame = vi.fn();
    vi.stubGlobal("requestAnimationFrame", frame);
    const { rerender } = render(Markdown, { source: "First" });
    await rerender({ source: "Second" });

    expect(frame).not.toHaveBeenCalled();
    expect(markdown.renderMarkdown).toHaveBeenLastCalledWith("Second");
  });
});
