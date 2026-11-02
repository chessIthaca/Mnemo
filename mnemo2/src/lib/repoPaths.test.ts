import { describe, expect, it } from "vitest";
import { parseRepoPath, repoPathTarget, splitRepoPaths } from "./repoPaths";

describe("parseRepoPath", () => {
  it("reads a plan document and its on-disk id", () => {
    expect(parseRepoPath(".coding/plans/fc536488.md")).toEqual({
      path: ".coding/plans/fc536488.md",
      line: null,
      kind: "plan",
      planId: "fc536488",
    });
  });

  it("keeps a plan's line while stripping it from the path", () => {
    expect(parseRepoPath(".coding/plans/7da65f3c.md:12")).toEqual({
      path: ".coding/plans/7da65f3c.md",
      line: 12,
      kind: "plan",
      planId: "7da65f3c",
    });
  });

  it("reads a file path with a line", () => {
    expect(parseRepoPath("src/agent/turn.rs:4352")).toEqual({
      path: "src/agent/turn.rs",
      line: 4352,
      kind: "file",
      planId: null,
    });
  });

  it("reads a file path without a line", () => {
    expect(parseRepoPath("mnemo2/src/lib/store.ts")).toEqual({
      path: "mnemo2/src/lib/store.ts",
      line: null,
      kind: "file",
      planId: null,
    });
  });

  it("reads a knowledge record's own path as a file", () => {
    expect(parseRepoPath(".coding/knowledge/spec/2027-02-14-spec-store.md")).toEqual({
      path: ".coding/knowledge/spec/2027-02-14-spec-store.md",
      line: null,
      kind: "file",
      planId: null,
    });
  });

  it("rejects URLs", () => {
    expect(parseRepoPath("https://example.com/docs/readme.md")).toBeNull();
    expect(parseRepoPath("http://example.com")).toBeNull();
    expect(parseRepoPath("mailto:someone@example.com")).toBeNull();
    expect(parseRepoPath("file:///C:/project/src/main.rs")).toBeNull();
  });

  it("rejects absolute paths", () => {
    expect(parseRepoPath("/home/user/project/src/main.rs")).toBeNull();
    expect(parseRepoPath("\\src\\main.rs")).toBeNull();
    expect(parseRepoPath("C:\\AgenticCoder\\src\\main.rs")).toBeNull();
    expect(parseRepoPath("~/project/src/main.rs")).toBeNull();
  });

  it("rejects a path that leaves the project", () => {
    expect(parseRepoPath("../src/main.rs")).toBeNull();
    expect(parseRepoPath("src/../../etc/passwd.rs")).toBeNull();
  });

  it("rejects bare words and references without an extension", () => {
    expect(parseRepoPath("workspace")).toBeNull();
    expect(parseRepoPath("agent.md")).toBeNull();
    expect(parseRepoPath("spec/2027-02-09-knowledge-as-files")).toBeNull();
  });

  it("rejects empty text and whitespace", () => {
    expect(parseRepoPath("")).toBeNull();
    expect(parseRepoPath("   ")).toBeNull();
    expect(parseRepoPath("src/agent turn.rs")).toBeNull();
  });
});

describe("splitRepoPaths", () => {
  it("returns one literal run when the text names no path", () => {
    expect(splitRepoPaths("Nothing to see here.")).toEqual([
      { text: "Nothing to see here.", ref: null },
    ]);
  });

  it("cuts the paths of running text out of their sentence", () => {
    const segments = splitRepoPaths("Touches `src/agent/turn.rs:4352` today.");
    expect(segments.map((segment) => segment.text)).toEqual([
      "Touches `",
      "src/agent/turn.rs:4352",
      "` today.",
    ]);
    expect(segments[1].ref).toMatchObject({ path: "src/agent/turn.rs", line: 4352 });
    expect(segments[0].ref).toBeNull();
    expect(segments[2].ref).toBeNull();
  });

  it("leaves a URL and a bare word alone", () => {
    const segments = splitRepoPaths("See https://example.com/a.md and workspace.");
    expect(segments.every((segment) => segment.ref === null)).toBe(true);
  });

  it("keeps the trailing punctuation out of the path", () => {
    const segments = splitRepoPaths("Stored in .coding/plans/fc536488.md.");
    expect(segments.map((segment) => segment.text)).toEqual([
      "Stored in ",
      ".coding/plans/fc536488.md",
      ".",
    ]);
    expect(segments[1].ref).toMatchObject({ kind: "plan", planId: "fc536488" });
  });
});

describe("repoPathTarget", () => {
  it("says which view a click opens", () => {
    expect(repoPathTarget(parseRepoPath(".coding/plans/fc536488.md")!)).toBe(
      "Open plan fc536488 in the Plans tab",
    );
    expect(repoPathTarget(parseRepoPath("src/agent/turn.rs:4352")!)).toBe(
      "Open src/agent/turn.rs at line 4352 in the Files tab",
    );
    expect(repoPathTarget(parseRepoPath("mnemo2/src/lib/store.ts")!)).toBe(
      "Open mnemo2/src/lib/store.ts in the Files tab",
    );
  });
});
