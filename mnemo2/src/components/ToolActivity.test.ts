import { render } from "@testing-library/svelte";
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
