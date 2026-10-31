import { describe, expect, it } from "vitest";
import { prettyArguments } from "./toolArguments";

describe("tool argument display", () => {
  it("shows a stringified nested argument decoded", () => {
    const raw = JSON.stringify({ files: JSON.stringify([{ path: "src/a.rs", start_line: 10 }]) });
    expect(prettyArguments(raw)).toBe(
      JSON.stringify({ files: [{ path: "src/a.rs", start_line: 10 }] }, null, 2),
    );
    expect(prettyArguments(raw)).not.toContain("\\");
  });

  it("keeps ordinary strings, malformed payloads and non-JSON text as sent", () => {
    expect(prettyArguments('{"command":"ls [a-z]"}')).toBe('{\n  "command": "ls [a-z]"\n}');
    const malformed = JSON.stringify({ files: '[{"path": "a.rs", "start_line": 4270"}]' });
    expect(prettyArguments(malformed)).toContain('\\"start_line\\": 4270\\"');
    expect(prettyArguments('{"files":["a.rs:1+2"')).toBe('{"files":["a.rs:1+2"');
  });
});
