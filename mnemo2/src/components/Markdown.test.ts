import { render } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Markdown from "./Markdown.svelte";

const markdown = vi.hoisted(() => ({
  renderMarkdown: vi.fn((source: string) => `<p>${source}</p>`),
}));

vi.mock("../lib/markdown", () => ({
  renderMarkdown: markdown.renderMarkdown,
}));

describe("Markdown", () => {
  beforeEach(() => {
    markdown.renderMarkdown.mockReset();
    markdown.renderMarkdown.mockImplementation((source: string) => `<p>${source}</p>`);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("renders the current source immediately and replaces it on updates", async () => {
    const { container, rerender } = render(Markdown, { source: "First" });
    expect(container.textContent).toContain("First");

    await rerender({ source: "Second" });

    expect(container.textContent).toContain("Second");
    expect(container.textContent).not.toContain("First");
    expect(markdown.renderMarkdown).toHaveBeenLastCalledWith("Second");
  });

  it("always renders the newest source rather than a stale fallback", async () => {
    const { container, rerender } = render(Markdown, { source: "one" });
    await rerender({ source: "two" });
    await rerender({ source: "three" });
    await rerender({ source: "four" });

    expect(container.textContent).toContain("four");
    expect(container.textContent).not.toContain("one");
    expect(markdown.renderMarkdown).toHaveBeenLastCalledWith("four");
  });

  it("does not re-render Markdown for unrelated prop changes", async () => {
    const { rerender } = render(Markdown, { source: "unchanged" });
    markdown.renderMarkdown.mockClear();

    await rerender({ source: "unchanged", onOpenLink: () => {} });

    expect(markdown.renderMarkdown).not.toHaveBeenCalled();
  });
});
