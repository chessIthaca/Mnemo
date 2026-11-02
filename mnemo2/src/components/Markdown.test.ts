import { fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RepoPathRef } from "../lib/repoPaths";
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

  it("leaves the paths of the text as plain text without the callback", () => {
    markdown.renderMarkdown.mockImplementation(() => "<p>See <code>src/agent/turn.rs</code>.</p>");
    const { container } = render(Markdown, { source: "See src/agent/turn.rs." });

    expect(container.querySelector("button.md-path")).toBeNull();
    expect(container.textContent).toContain("src/agent/turn.rs");
  });

  it("makes inline-code and plain-text paths clickable when given the callback", async () => {
    markdown.renderMarkdown.mockImplementation(
      () => "<p>See <code>src/agent/turn.rs:4352</code> and the plan .coding/plans/fc536488.md.</p>",
    );
    const opened: RepoPathRef[] = [];
    const { container } = render(Markdown, {
      source: "See src/agent/turn.rs:4352 and the plan .coding/plans/fc536488.md.",
      onOpenPath: (ref: RepoPathRef) => opened.push(ref),
    });

    await waitFor(() => expect(container.querySelectorAll("button.md-path")).toHaveLength(2));
    const buttons = [...container.querySelectorAll<HTMLButtonElement>("button.md-path")];
    expect(buttons.map((button) => button.textContent)).toEqual([
      "src/agent/turn.rs:4352",
      ".coding/plans/fc536488.md",
    ]);
    expect(buttons[0].title).toBe("Open src/agent/turn.rs at line 4352 in the Files tab");
    expect(buttons[1].title).toBe("Open plan fc536488 in the Plans tab");

    await fireEvent.click(buttons[0]);
    expect(opened).toEqual([{ path: "src/agent/turn.rs", line: 4352, kind: "file", planId: null }]);
  });
});
