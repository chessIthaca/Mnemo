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
});
