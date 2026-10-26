import { describe, expect, it } from "vitest";
import { toolSummary } from "./toolSummary";

describe("human tool summaries", () => {
  it("uses the shared action phrases for real tool arguments", () => {
    expect(toolSummary("shell", '{"purpose":"running tests"}')).toBe("Running tests");
    expect(toolSummary("read_files", '{"files":[{"path":"a.rs"},{"path":"b.rs"}]}')).toBe("Reading 2 files");
    expect(toolSummary("search", '{"pattern":"ToolActivity"}')).toBe('Searching for "ToolActivity"');
    expect(toolSummary("write_file", "{}")).toBe("Writing files");
  });

  it("describes Mnemo2 team activity without claiming a result", () => {
    expect(toolSummary("team_dispatch", '{"brief":"Inspect the parser"}')).toBe("Delegating: Inspect the parser");
    expect(toolSummary("team_review", "{}")).toBe("Reviewing a worker");
    expect(toolSummary("team_check", "{}")).toBe("Recording a worker check");
    expect(toolSummary("team_merge", "{}")).toBe("Merging a worker branch");
  });

  it("falls back honestly for partial arguments and unknown tools", () => {
    expect(toolSummary("shell", '{"command":')).toBe("Using Shell");
    expect(toolSummary("mcp__github__search", "{}")).toBe("Using Github search");
  });

  it("keeps a long or multiline headline compact", () => {
    expect(toolSummary("team_dispatch", JSON.stringify({ brief: `first line\nsecond line` }))).toBe("Delegating: first line");
    expect(toolSummary("team_dispatch", JSON.stringify({ brief: "x".repeat(160) })).length).toBe(120);
  });
});
