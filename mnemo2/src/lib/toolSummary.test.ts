import { describe, expect, it } from "vitest";
import { toolHeadline, toolSummary } from "./toolSummary";

describe("human tool summaries", () => {
  it("uses the shared action phrases for real tool arguments", () => {
    expect(toolSummary("shell", '{"purpose":"running tests"}')).toBe("Running tests");
    expect(toolSummary("read_files", '{"files":[{"path":"a.rs"},{"path":"b.rs"}]}')).toBe("Reading a.rs, b.rs");
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

describe("file-named headlines", () => {
  it("names the files a read, edit or write touches", () => {
    expect(toolSummary("read_files", '{"files":["a.ts"]}')).toBe("Reading a.ts");
    expect(toolSummary("read_files", '{"files":["a.ts","b.ts"]}')).toBe("Reading a.ts, b.ts");
    expect(toolSummary("read_files", '{"path":"src/a.ts"}')).toBe("Reading a.ts");
    expect(toolSummary("file_edit", '{"path":"src/a.ts","old_string":"x","new_string":"y"}')).toBe("Editing a.ts");
    expect(toolSummary("multi_edit", '{"files":[{"path":"a.ts"},{"path":"b.ts"},{"path":"c.ts"}]}')).toBe(
      "Editing a.ts, b.ts, c.ts",
    );
    expect(toolSummary("file_write", '{"path":"src/new.ts","content":"hi"}')).toBe("Writing new.ts");
    expect(toolSummary("write_file", '{"path":"notes/log.md"}')).toBe("Writing log.md");
    expect(toolSummary("file_append", '{"path":"log.txt"}')).toBe("Appending to log.txt");
    expect(toolSummary("file_write", '{"path":"log.txt","mode":"append"}')).toBe("Appending to log.txt");
  });

  it("caps the headline at three file names and says how many more there are", () => {
    expect(toolSummary("read_files", '{"files":["a.ts","b.ts","c.ts","d.ts","e.ts"]}')).toBe(
      "Reading a.ts, b.ts, c.ts +2 more",
    );
    expect(toolSummary("multi_edit", '{"files":[{"path":"a"},{"path":"b"},{"path":"c"},{"path":"d"}]}')).toBe(
      "Editing a, b, c +1 more",
    );
  });

  it("keeps one name for the same file in either casing", () => {
    expect(toolSummary("read_files", '{"files":[{"path":"src/a.ts"},{"path":"SRC/A.TS"}]}')).toBe("Reading a.ts");
  });

  it("salvages complete paths from a still-streaming argument fragment", () => {
    // compact string entries and object entries alike, cut mid-second-file
    expect(toolSummary("read_files", '{"files":["src/a.ts","src/b')).toBe("Reading a.ts");
    expect(toolSummary("multi_edit", '{"files":[{"path":"a.ts"},{"path":"b')).toBe("Editing a.ts");
    expect(toolSummary("file_write", '{"path":"src/a.ts","content":"ste')).toBe("Writing a.ts");
    // an ops anchor's old_string is not a path
    expect(toolSummary("multi_edit", '{"files":[{"path":"a.ts","ops":[{"old_string":"x"')).toBe("Editing a.ts");
    // no complete path yet: the ordinary fallback chain, as before
    expect(toolSummary("read_files", '{"files":["a')).toBe("Using Read files");
    expect(toolSummary("file_edit", '{"path":"a')).toBe("Using File edit");
    expect(toolSummary("read_files", '{"files":[]}')).toBe("Reading files");
  });

  it("carries the full paths as the tooltip when the headline names files", () => {
    expect(toolHeadline("read_files", '{"files":["src/a.ts","src/b.ts"]}')).toEqual({
      text: "Reading a.ts, b.ts",
      title: "src/a.ts, src/b.ts",
    });
    expect(toolHeadline("read_files", '{"files":["a","b","c","d"]}')).toEqual({
      text: "Reading a, b, c +1 more",
      title: "a, b, c, d",
    });
    // without paths the headline doubles as its own tooltip, as before
    expect(toolHeadline("write_file", "{}")).toEqual({ text: "Writing files", title: "Writing files" });
  });
});