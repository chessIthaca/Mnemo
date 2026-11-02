import { describe, expect, it } from "vitest";
import type { LeadToolResult } from "./types";
import { editDiffSections, writeDiffSections } from "./toolDiff";

/** A successful result — the shape a live call carries, `data` optional. */
const ok = (output = "edited", data?: unknown): LeadToolResult => ({
  success: true,
  output,
  ...(data === undefined ? {} : { data }),
});

describe("edit diffs", () => {
  it("uses the result's structured diff when the tool sent one", () => {
    const sections = editDiffSections(
      "file_edit",
      '{"path":"a.rs","old_string":"old text","new_string":"new text"}',
      ok("Edited a.rs", { diff: "--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old text\n+new text" }),
    );
    expect(sections).toHaveLength(1);
    expect(sections![0]).toMatchObject({ path: null, source: "result", note: null });
    const text = sections![0].lines.map((line) => `${line.type}:${line.text}`).join("\n");
    expect(text).toContain("meta:--- a/a.rs");
    expect(text).toContain("meta:+++ b/a.rs");
    expect(text).toContain("remove:old text");
    expect(text).toContain("add:new text");
    expect(text).not.toContain("old_string");
  });

  it("rebuilds a diff from old_string/new_string when no payload exists (replayed transcripts)", () => {
    const sections = editDiffSections(
      "file_edit",
      '{"path":"a.rs","old_string":"old text","new_string":"new text"}',
      ok("Edited a.rs"),
    );
    expect(sections).toHaveLength(1);
    expect(sections![0]).toMatchObject({ path: "a.rs", source: "arguments" });
    expect(sections![0].note).toBeTruthy();
    expect(sections![0].lines.filter((line) => line.type === "remove")).toEqual([{ type: "remove", text: "old text" }]);
    expect(sections![0].lines.filter((line) => line.type === "add")).toEqual([{ type: "add", text: "new text" }]);
  });

  it("treats a missing new_string as a deletion and an unchanged pair as no diff", () => {
    const sections = editDiffSections("file_edit", '{"path":"a.rs","old_string":"gone"}', ok());
    expect(sections![0].lines).toEqual([{ type: "remove", text: "gone" }]);
    expect(editDiffSections("file_edit", '{"path":"a.rs","old_string":"same","new_string":"same"}', ok())).toBeNull();
  });

  it("builds one section per multi_edit file from its anchor ops", () => {
    const args = JSON.stringify({
      files: [
        { path: "a.ts", ops: [{ old_string: "one", new_string: "uno" }] },
        { path: "b.ts", ops: [{ old_string: "two", new_string: "dos" }, { old_string: "three", new_string: "tres" }] },
        { path: "c.ts", ops: ["d10-12"] },
      ],
    });
    const sections = editDiffSections("multi_edit", args, ok("Edited 2 files"));
    expect(sections).toHaveLength(2);
    expect(sections!.map((section) => section.path)).toEqual(["a.ts", "b.ts"]);
    // two anchor pairs in one file are separated by a hunk-style divider
    expect(sections![1].lines.filter((line) => line.type === "meta")).toEqual([{ type: "meta", text: "@@ ··· @@" }]);
  });

  it("falls back to nothing for line-range edits and compact-only ops", () => {
    expect(editDiffSections("file_edit", '{"path":"a.rs","lines":[2,5],"new_string":"x"}', ok())).toBeNull();
    expect(editDiffSections("multi_edit", '{"files":[{"path":"c.ts","ops":["d10-12"]}]}', ok())).toBeNull();
    expect(editDiffSections("file_edit", "still streaming", ok())).toBeNull();
    expect(editDiffSections("file_edit", '{"path":"a.rs"}', ok("Edited a.rs"))).toBeNull();
  });

  it("never renders a diff for a failed edit", () => {
    expect(
      editDiffSections("file_edit", '{"path":"a.rs","old_string":"x","new_string":"y"}', {
        success: false,
        output: "old_string not found",
      }),
    ).toBeNull();
  });
});

describe("write diffs", () => {
  it("shows the written content as added lines under its path", () => {
    // Two written lines — a template literal, so the content needs no escaped newline.
    const content = `one
two`;
    const sections = writeDiffSections("file_write", JSON.stringify({ path: "src/new.ts", content }));
    expect(sections).toHaveLength(1);
    expect(sections![0]).toMatchObject({ path: "src/new.ts", source: "arguments", note: null });
    expect(sections![0].lines).toEqual([
      { type: "add", text: "one" },
      { type: "add", text: "two" },
    ]);
  });

  it("labels appended content, and an empty write is one empty added line", () => {
    expect(writeDiffSections("file_write", '{"path":"log.txt","content":"more","mode":"append"}')).toMatchObject([
      { note: "Appended content" },
    ]);
    expect(writeDiffSections("file_append", '{"path":"log.txt","content":"more"}')).toMatchObject([
      { note: "Appended content" },
    ]);
    expect(writeDiffSections("write_file", '{"path":"a.ts","content":"x"}')).toMatchObject([{ note: null }]);
    expect(writeDiffSections("file_write", '{"path":"a.ts","content":""}')![0].lines).toEqual([{ type: "add", text: "" }]);
  });

  it("returns null without a path or content", () => {
    expect(writeDiffSections("file_write", '{"path":"a.ts"}')).toBeNull();
    expect(writeDiffSections("file_write", "{}")).toBeNull();
    expect(writeDiffSections("file_write", "not json")).toBeNull();
  });
});