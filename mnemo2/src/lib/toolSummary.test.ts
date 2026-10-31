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

  it("names the worker a team call acts on", () => {
    const names = (id: string) => (id === "w1" ? "ghostai0001" : id);
    expect(toolSummary("team_merge", '{"worker_id":"w1"}', names)).toBe("Merging ghostai0001’s branch");
    expect(toolSummary("team_review", '{"worker_id":"renderer0001","summary":"ok"}')).toBe("Reviewing renderer0001’s work");
    expect(toolSummary("team_check", '{"worker_id":"w1"}', names)).toBe("Recording a check for ghostai0001");
    expect(toolSummary("team_dispatch", '{"worker_id":"gamecore0001","brief":"Build the loop"}')).toBe(
      "Delegating to gamecore0001: Build the loop",
    );
    // While the arguments still stream, a complete worker_id already names
    // it; an unfinished string is never shown half-typed.
    expect(toolSummary("team_dispatch", '{"worker_id":"gamecore0001","brief":"Build the l')).toBe(
      "Delegating to gamecore0001",
    );
    expect(toolSummary("team_dispatch", '{"worker_id":"gamec')).toBe("Delegating a task");
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
