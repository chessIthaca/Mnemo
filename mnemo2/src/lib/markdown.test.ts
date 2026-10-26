import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./markdown";

describe("lead Markdown rendering", () => {
  it("renders tables, fenced code, and safe external links", () => {
    const html = renderMarkdown([
      "| Area | Owner |",
      "| --- | --- |",
      "| State | Ana |",
      "",
      "```ts",
      "const ready = true;",
      "```",
      "",
      "[Docs](https://example.com/docs)",
    ].join("\n"));

    expect(html).toContain("<table>");
    expect(html).toContain("language-ts");
    expect(html).toContain("data-copy-code");
    expect(html).toContain('target="_blank"');
    expect(html).toContain("noreferrer noopener");
  });

  it("does not emit raw script HTML or javascript URLs", () => {
    const html = renderMarkdown(
      "Hello <script>alert(1)</script> [bad](javascript:alert(1))",
    );
    expect(html).not.toContain("<script>");
    expect(html).not.toContain('href="javascript:');
  });

  it("escapes code before highlighting when a language is unknown", () => {
    const html = renderMarkdown("```not-a-language\n<unsafe>\n```");
    expect(html).toContain("&lt;unsafe&gt;");
    expect(html).not.toContain("<unsafe>");
  });

  it("renders GFM task lists as disabled checkboxes rather than literal markers", () => {
    const container = document.createElement("div");
    container.innerHTML = renderMarkdown("- [x] done thing\n- [ ] pending thing");
    const items = container.querySelectorAll("li");
    const checkboxes = container.querySelectorAll("input");

    expect(container.querySelector("ul")?.classList.contains("contains-task-list")).toBe(true);
    expect(items).toHaveLength(2);
    expect(checkboxes).toHaveLength(2);
    expect(items[0].classList.contains("task-list-item")).toBe(true);
    expect(items[0].textContent?.trim()).toBe("done thing");
    expect(items[1].textContent?.trim()).toBe("pending thing");
    expect(checkboxes[0].checked).toBe(true);
    expect(checkboxes[0].disabled).toBe(true);
    expect(checkboxes[0].getAttribute("aria-label")).toBe("Completed task");
    expect(checkboxes[1].checked).toBe(false);
    expect(checkboxes[1].disabled).toBe(true);
    expect(checkboxes[1].getAttribute("aria-label")).toBe("Incomplete task");
    expect(container.textContent).not.toContain("[x]");
    expect(container.textContent).not.toContain("[ ]");
  });

  it("recognizes only actual task markers and preserves Markdown in their labels", () => {
    const container = document.createElement("div");
    container.innerHTML = renderMarkdown(
      "- [x] **done**\n- [ ] `pending`\n\n- **[x]** emphasis\n- `[x]` code\n- \\[x\\] escaped",
    );
    const tasks = container.querySelectorAll("li.task-list-item");

    expect(tasks).toHaveLength(2);
    expect(tasks[0].querySelector("strong")?.textContent).toBe("done");
    expect(tasks[1].querySelector("code")?.textContent).toBe("pending");
    expect(container.querySelectorAll("input")).toHaveLength(2);
    expect(container.textContent).toContain("[x]");
  });

  it("renders CommonMark emphasis and block structures plus GFM strikethrough", () => {
    const html = renderMarkdown(
      "****Verify builds/tests****\n\n*italic* and ~~removed~~\n\n#### Heading\n\n> Quoted\n\n---",
    );
    expect(html).toContain("<strong><strong>Verify builds/tests</strong></strong>");
    expect(html).toContain("<em>italic</em>");
    expect(html).toMatch(/<(?:del|s)>removed<\/(?:del|s)>/);
    expect(html).toContain("<h4>Heading</h4>");
    expect(html).toContain("<blockquote>");
    expect(html).toContain("<hr>");
  });

  it("autolinks URLs and distinguishes hard breaks from soft newlines", () => {
    const html = renderMarkdown("see https://example.com/x now\nline two  \nline three");
    expect(html).toContain('<a href="https://example.com/x"');
    expect(html.match(/<br>/g)).toHaveLength(1);
    expect(renderMarkdown("line one\nline two")).not.toContain("<br>");
  });

  it("retains safe table alignment", () => {
    const html = renderMarkdown("| Left | Right |\n| :--- | ---: |\n| a | b |");
    expect(html).toContain('style="text-align:right"');
  });

  it("preserves literal punctuation instead of applying typographic replacements", () => {
    const html = renderMarkdown('"Quoted" -- wait ... (c)');
    expect(html).toContain('"Quoted" -- wait ... (c)');
    expect(html).not.toContain("“");
    expect(html).not.toContain("…");
    expect(html).not.toContain("©");
  });
});
