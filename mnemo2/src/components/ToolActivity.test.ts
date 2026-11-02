import { fireEvent, render, waitFor } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";
import type { LeadEntry } from "../lib/types";
import ToolActivity from "./ToolActivity.svelte";

type ToolEntry = Extract<LeadEntry, { kind: "tool" }>;

function running(liveOutput: string): ToolEntry {
  return {
    id: "tool-1",
    kind: "tool",
    time: "now",
    toolCallId: "call-1",
    toolName: "shell",
    arguments: '{"command":"npm test"}',
    liveOutput,
    result: null,
  } as ToolEntry;
}

function preview(container: HTMLElement) {
  const box = container.querySelector<HTMLElement>(".tool-live");
  return {
    rows: box?.style.getPropertyValue("--live-rows"),
    lines: [...(box?.querySelectorAll(".term-line") ?? [])].map((line) => line.textContent),
  };
}

describe("ToolActivity live preview", () => {
  it("grows with the output up to five rows and never shrinks", async () => {
    const { container, rerender } = render(ToolActivity, { entry: running("one\ntwo\n") });
    expect(preview(container)).toEqual({ rows: "2", lines: ["one", "two"] });

    await rerender({ entry: running("1\n2\n3\n4\n5\n6\n7\n") });
    expect(preview(container)).toEqual({ rows: "5", lines: ["3", "4", "5", "6", "7"] });

    // A progress bar redraws one line: the box keeps its height.
    await rerender({ entry: running("50%\r60%") });
    expect(preview(container)).toEqual({ rows: "5", lines: ["60%"] });
  });

  it("renders terminal colors instead of raw escape codes", () => {
    const { container } = render(ToolActivity, {
      entry: running("\x1b[32m✓\x1b[39m 12 passed\n[31m✗[39m 1 failed\n"),
    });
    const text = container.querySelector(".tool-live")?.textContent ?? "";
    expect(text).toContain("✓ 12 passed");
    expect(text).toContain("✗ 1 failed");
    expect(text).not.toMatch(/\[\d+m/);
    const check = [...container.querySelectorAll<HTMLElement>(".tool-live span[style]")].find(
      (span) => span.textContent === "✓",
    );
    expect(check?.getAttribute("style")).toContain("color");
  });
});
describe("ToolActivity expanded bodies", () => {
  /** One tool entry, overridable per test. */
  function entry(overrides: Partial<ToolEntry> = {}): ToolEntry {
    return {
      id: "tool-1",
      kind: "tool",
      time: "now",
      toolCallId: "call-1",
      toolName: "file_edit",
      arguments: "{}",
      liveOutput: "",
      result: null,
      ...overrides,
    } as ToolEntry;
  }

  async function expand(container: HTMLElement) {
    const summary = container.querySelector<HTMLElement>("summary");
    if (summary === null) throw new Error("the tool row has no summary");
    await fireEvent.click(summary);
    await waitFor(() => expect(container.querySelector("details")?.open).toBe(true));
  }

  it("renders an edit's structured diff, keeping the raw call behind the toggle", async () => {
    const { container } = render(ToolActivity, {
      entry: entry({
        toolName: "file_edit",
        arguments: '{"path":"a.rs","old_string":"old text","new_string":"new text"}',
        result: {
          success: true,
          output: "Edited a.rs",
          data: { diff: "--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old text\n+new text" },
        },
      }),
    });
    await expand(container);
    await waitFor(() => expect(container.querySelectorAll(".tool-diff-line").length).toBeGreaterThan(0));
    expect(
      [...container.querySelectorAll('.tool-diff-line[data-type="remove"]')].map((node) => node.textContent),
    ).toEqual(["−old text"]);
    expect(
      [...container.querySelectorAll('.tool-diff-line[data-type="add"]')].map((node) => node.textContent),
    ).toEqual(["+new text"]);
    // the Arguments JSON is no longer the body…
    expect(container.textContent).not.toContain("old_string");
    // …but it stays reachable behind the Show raw call toggle
    const toggle = container.querySelector<HTMLButtonElement>(".tool-raw-toggle");
    expect(toggle).toBeTruthy();
    await fireEvent.click(toggle!);
    await waitFor(() => expect(container.textContent).toContain("old_string"));
    expect(container.textContent).toContain("Edited a.rs");
  });

  it("reconstructs an edit's diff from its arguments when no payload exists", async () => {
    const { container } = render(ToolActivity, {
      entry: entry({
        toolName: "file_edit",
        arguments: '{"path":"a.rs","old_string":"old text","new_string":"new text"}',
        // a replayed transcript rebuilds results from the message text — no data
        result: { success: true, output: "Edited a.rs" },
      }),
    });
    await expand(container);
    await waitFor(() => expect(container.querySelector(".tool-diff")).toBeTruthy());
    expect(container.textContent).toContain("a.rs");
    expect(container.textContent).toContain("Reconstructed from the call arguments");
    expect(
      [...container.querySelectorAll('.tool-diff-line[data-type="remove"]')].map((node) => node.textContent),
    ).toEqual(["−old text"]);
  });

  it("renders a write's content as added lines under its path", async () => {
    const { container } = render(ToolActivity, {
      entry: entry({
        toolName: "file_write",
        arguments: '{"path":"src/greet.ts","content":"export const hi = 1;"}',
        result: { success: true, output: "Wrote src/greet.ts" },
      }),
    });
    await expand(container);
    await waitFor(() => expect(container.querySelector(".tool-diff")).toBeTruthy());
    expect(container.querySelector(".tool-diff-file code")?.textContent).toBe("src/greet.ts");
    expect(container.querySelectorAll('.tool-diff-line[data-type="remove"]').length).toBe(0);
    expect(
      [...container.querySelectorAll('.tool-diff-line[data-type="add"]')].map((node) => node.textContent),
    ).toEqual(["+export const hi = 1;"]);
  });

  it("lists the files and line ranges a read returned, with the output behind the toggle", async () => {
    const output = [
      "=== mnemo2/src/lib/toolSummary.ts (lines 1-44 of 65) ===",
      "1  import { toolHeadline } from …",
      "",
      "=== mnemo2/src/lib/types.ts (lines 272-284 of 316) ===",
      "272  export interface LeadToolResult …",
      "",
      "=== mnemo2/missing.rs (error) ===",
      "not found",
    ].join("\n");
    const { container } = render(ToolActivity, {
      entry: entry({
        toolName: "read_files",
        arguments:
          '{"files":[{"path":"mnemo2/src/lib/toolSummary.ts","start_line":1,"max_lines":44},{"path":"mnemo2/src/lib/types.ts","start_line":272,"max_lines":13}]}',
        result: { success: true, output },
      }),
    });
    await expand(container);
    await waitFor(() => expect(container.querySelectorAll(".tool-reads li").length).toBe(3));
    const rows = [...container.querySelectorAll(".tool-reads li")].map((li) => li.textContent ?? "");
    expect(rows[0]).toContain("mnemo2/src/lib/toolSummary.ts");
    expect(rows[0]).toContain("lines 1–44 of 65");
    expect(rows[1]).toContain("mnemo2/src/lib/types.ts");
    expect(rows[1]).toContain("lines 272–284 of 316");
    expect(rows[2]).toContain("not found");
    // the raw numbered content is not the body — it stays behind the toggle
    expect(container.textContent).not.toContain("272  export interface");
    await fireEvent.click(container.querySelector<HTMLButtonElement>(".tool-raw-toggle")!);
    await waitFor(() => expect(container.textContent).toContain("272  export interface"));
  });

  it("renders a shell call as a command block with split output and its exit code", async () => {
    const { container } = render(ToolActivity, {
      entry: entry({
        toolName: "shell",
        arguments: '{"command":"cargo test","purpose":"running tests"}',
        result: {
          success: true,
          output: "test result: ok. 4 passed\n[stderr]\nwarning: unused import\n[exit code: 0]",
        },
      }),
    });
    await expand(container);
    await waitFor(() => expect(container.querySelector(".tool-command")?.textContent).toBe("cargo test"));
    expect(container.textContent).toContain("purpose: running tests");
    expect(container.textContent).toContain("test result: ok. 4 passed");
    expect(container.querySelector(".tool-shell-err")?.textContent).toContain("warning: unused import");
    expect(container.querySelector(".tool-exit")?.getAttribute("data-ok")).toBe("true");
    // the raw JSON call moved behind the Show raw call toggle
    expect(container.textContent).not.toContain('"command": "cargo test"');
    await fireEvent.click(container.querySelector<HTMLButtonElement>(".tool-raw-toggle")!);
    await waitFor(() => expect(container.textContent).toContain('"command": "cargo test"'));
  });

  it("shows a failed shell's error output with its exit code", async () => {
    const { container } = render(ToolActivity, {
      entry: entry({
        toolName: "shell",
        arguments: '{"command":"cargo build"}',
        result: { success: false, output: "error: could not compile\n[stderr]\ncargo: not found\n[exit code: 101]" },
      }),
    });
    await expand(container);
    await waitFor(() => expect(container.querySelector(".tool-shell-err")?.textContent).toContain("cargo: not found"));
    expect(container.textContent).toContain("Error");
    expect(container.querySelector(".tool-exit")?.getAttribute("data-ok")).toBe("false");
  });

  it("keeps the generic Arguments/Result view for other tools", async () => {
    const { container } = render(ToolActivity, {
      entry: entry({
        toolName: "git_read",
        arguments: '{"op":"log"}',
        result: { success: true, output: "abc123 fix: parser\n" },
      }),
    });
    await expand(container);
    await waitFor(() => expect(container.textContent).toContain("abc123 fix: parser"));
    expect(container.textContent).toContain('"op": "log"');
    expect(container.querySelector(".tool-raw-toggle")).toBeNull();
    expect(container.querySelector(".tool-diff")).toBeNull();
  });

  it("keeps a failed write's error text in view without a content diff", async () => {
    const { container } = render(ToolActivity, {
      entry: entry({
        toolName: "file_write",
        arguments: '{"path":"a.ts","content":"x"}',
        result: { success: false, output: "permission denied" },
      }),
    });
    await expand(container);
    // "Error" exists only in the expanded body — waiting for it bridges
    // jsdom's asynchronous details-toggle event.
    await waitFor(() => expect(container.textContent).toContain("Error"));
    expect(container.textContent).toContain("permission denied");
    expect(container.querySelector(".tool-diff")).toBeNull();
  });
});