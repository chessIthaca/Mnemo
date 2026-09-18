// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Unit tests for the tool-card presentation helpers
 * (`frontend/src/lib/toolCardPaths.ts`): the file-path extractor driving the
 * clickable file-name links in the ToolCard header, plus the readable-IO
 * parsing powering the expanded call-detail body — shell calls
 * (`shellCallFromArgs` / `parseShellOutput`, backlog 8c1d8a47), file_edit
 * diffs (`fileEditDiff`), and read_files line ranges
 * (`parseReadFilesSections` / `argPathLines`, backlog cb3461fe).
 */

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

import { argLabel, argPathLines, argPaths, basename, browserResultInfo, buildPathChips, dedupePaths, displayName, fileEditDiff, graphCallLabel, isBrowserToolName, liveTailPreview, memorySearchLabel, parseReadFilesSections, parseShellOutput, searchResultInfo, shellCallFromArgs, toolErrorSummary, webFetchLabel, webFetchUrl } from "./toolCardPaths";

describe("basename", () => {
  it("returns the last path segment (forward + back slashes)", () => {
    expect(basename("src/agent/context.rs")).toBe("context.rs");
    expect(basename("src\\agent\\context.rs")).toBe("context.rs");
    expect(basename("context.rs")).toBe("context.rs");
  });
});

describe("argPaths", () => {
  it("extracts a top-level path", () => {
    expect(argPaths('{"path":"src/main.rs"}', "file_read")).toEqual(["src/main.rs"]);
  });

  it("extracts a top-level file field", () => {
    expect(argPaths('{"file":"src/lib.rs"}', "some_tool")).toEqual(["src/lib.rs"]);
  });

  it("extracts each read_files spec path", () => {
    const args = JSON.stringify({
      files: [{ path: "a.rs" }, { path: "b.rs", start_line: 1 }, { path: "c.rs" }],
    });
    expect(argPaths(args, "read_files")).toEqual(["a.rs", "b.rs", "c.rs"]);
  });

  it("read_files single-file `path` shorthand falls back to the top-level path", () => {
    // The Rust tool accepts {"path": "..."} (absorbed from the old file_read);
    // the card must still yield a clickable link for that shape.
    expect(argPaths('{"path":"src/main.rs"}', "read_files")).toEqual(["src/main.rs"]);
  });

  it("read_files `files` array wins over a top-level `path` when both present", () => {
    // The shorthand only applies when there is no batch — an explicit `files`
    // array is the batch and must take precedence (mirrors the Rust tool).
    const args = JSON.stringify({ path: "ignored.rs", files: [{ path: "a.rs" }] });
    expect(argPaths(args, "read_files")).toEqual(["a.rs"]);
  });

  it("maps write_review_report's bare filename to .coding/reviews/", () => {
    expect(argPaths('{"path":"2026-11-23-review.md"}', "write_review_report")).toEqual([
      ".coding/reviews/2026-11-23-review.md",
    ]);
    // A path that already carries a directory is left alone.
    expect(argPaths('{"path":"sub/report.md"}', "write_review_report")).toEqual(["sub/report.md"]);
  });

  it("returns [] for label-only tools (shell purpose, git subcommand, git_read op, search pattern, spawn name, skill name)", () => {
    expect(argPaths('{"purpose":"running tests"}', "shell")).toEqual([]);
    expect(argPaths('{"subcommand":"status"}', "git")).toEqual([]);
    // git_read's path is a log/diff FILTER, not an openable file — the op
    // chip (argLabel) is the meaningful part.
    expect(argPaths('{"op":"log","path":"src/lib"}', "git_read")).toEqual([]);
    expect(argPaths('{"pattern":"WorkflowState","glob":"**/*.rs"}', "search")).toEqual([]);
    expect(argPaths('{"name":"reviewer"}', "spawn_agent")).toEqual([]);
    // Skill tools: skill_start's `skill` and skill_create's `name` are skill
    // names (file stems under .coding/skills/), never files to open.
    expect(argPaths('{"skill":"merge_to_main"}', "skill_start")).toEqual([]);
    expect(
      argPaths(
        '{"name":"deploy_checklist","tools":["git"],"prompt":"Run the deploy checklist"}',
        "skill_create",
      ),
    ).toEqual([]);
  });

  it("returns [] for malformed JSON or no path field", () => {
    expect(argPaths("{bad json}", "file_read")).toEqual([]);
    expect(argPaths('{"foo":"bar"}', "file_read")).toEqual([]);
  });

  // Regression (2026-12 user report): ToolInvocation.args streams in via
  // tool_call_arg_delta fragments, so an interrupted/aborted call stores
  // TRUNCATED JSON forever — JSON.parse fails and the card stayed nameless
  // even though the path literal sits right inside the fragment.
  it("salvages a top-level path literal from truncated JSON args", () => {
    // The exact shape from the report: single-file read_files shorthand cut
    // off before the closing brace.
    expect(
      argPaths('{"path": ".coding/reviews/2026-12-r9-r10-model-switch-review.md"', "read_files"),
    ).toEqual([".coding/reviews/2026-12-r9-r10-model-switch-review.md"]);
  });

  it("salvages the first spec path from a truncated batched read_files args", () => {
    expect(argPaths('{"files": [{"path": "a.rs"}, {"path":', "read_files")).toEqual(["a.rs"]);
  });

  it("does not salvage for label-only tools even when a path literal is present", () => {
    // Early-excluded tools (git_read's path is a filter, shell, …) must never
    // gain a link chip — including via the salvage path.
    expect(argPaths('{"op":"log","path":"src/li', "git_read")).toEqual([]);
    expect(argPaths('{"purpose":"readin","path":"x.rs"}', "shell")).toEqual([]);
    // Truncated args whose COMPLETE path literal would salvage: the exclusion
    // must still win (regression: the salvage ran before the toolName check).
    expect(argPaths('{"op":"log","path":"src/lib.rs","limit":20', "git_read")).toEqual([]);
    expect(argPaths('{"purpose":"run tests","path":"x.rs"', "shell")).toEqual([]);
    // A skill_create fragment carrying a COMPLETE path literal must not
    // salvage one either: `name`/`prompt` are label data, not files.
    expect(
      argPaths(
        '{"name":"deploy_checklist","prompt":"see .coding/plans/x.md","path":"x.rs"',
        "skill_create",
      ),
    ).toEqual([]);
  });

  it("still returns [] when truncated args contain no path/file literal", () => {
    expect(argPaths("{not json at all}", "file_read")).toEqual([]);
    expect(argPaths('{"pattern":"Workflo', "search")).toEqual([]);
  });
});

describe("graphCallLabel", () => {
  it("graph_search: quotes the query", () => {
    expect(graphCallLabel("graph_search", '{"query":"GraphView"}')).toBe('"GraphView"');
  });

  it("graph_context / graph_impact: prefer name over id", () => {
    const args = JSON.stringify({
      name: "MemoryStore",
      id: "src/memory/mod.rs::MemoryStore::253",
    });
    expect(graphCallLabel("graph_context", args)).toBe("MemoryStore");
    expect(graphCallLabel("graph_impact", args)).toBe("MemoryStore");
  });

  it("graph_context / graph_impact: shorten a file::name::line id to its name segment", () => {
    const args = JSON.stringify({ id: "src/agent/turn.rs::run_turn::340" });
    expect(graphCallLabel("graph_context", args)).toBe("run_turn");
    expect(graphCallLabel("graph_impact", args)).toBe("run_turn");
  });

  it("graph_path: renders from → to with id shortening", () => {
    const args = JSON.stringify({
      from: "src/main.rs::main::12",
      to: "db_connect",
    });
    expect(graphCallLabel("graph_path", args)).toBe("main → db_connect");
  });

  it("unknown tools and malformed JSON return null", () => {
    expect(graphCallLabel("search", '{"query":"x"}')).toBeNull();
    expect(graphCallLabel("file_read", '{"path":"a.rs"}')).toBeNull();
    expect(graphCallLabel("graph_search", "{bad json}")).toBeNull();
  });

  it("missing key fields return null (no query / no id / one-sided path)", () => {
    expect(graphCallLabel("graph_search", "{}")).toBeNull();
    expect(graphCallLabel("graph_context", "{}")).toBeNull();
    expect(graphCallLabel("graph_path", '{"from":"main"}')).toBeNull();
  });
});

describe("memorySearchLabel", () => {
  it("quotes the bare query", () => {
    expect(memorySearchLabel('{"query":"resize seam"}')).toBe('"resize seam"');
  });

  it("appends scope suffixes: record_type, prefix, limit", () => {
    expect(memorySearchLabel('{"query":"resize seam","record_type":"bug"}')).toBe(
      '"resize seam" · bug',
    );
    expect(
      memorySearchLabel('{"query":"x","prefix":"BUG:","limit":5}'),
    ).toBe('"x" · BUG: · limit 5');
  });

  it("falls back to tier when record_type is absent", () => {
    expect(memorySearchLabel('{"query":"x","tier":"semantic"}')).toBe('"x" · semantic');
    // record_type wins when both are present.
    expect(memorySearchLabel('{"query":"x","tier":"semantic","record_type":"plan"}')).toBe(
      '"x" · plan',
    );
  });

  it("a query-less browse shows its filters", () => {
    expect(memorySearchLabel('{"tier":"semantic"}')).toBe("browse · semantic");
  });

  it("returns null for malformed JSON, no query and no filters, or blank query", () => {
    expect(memorySearchLabel("{bad json}")).toBeNull();
    expect(memorySearchLabel("{}")).toBeNull();
    expect(memorySearchLabel('{"query":"  "}')).toBeNull();
  });
});

/** Pins the search/search_read output → card-metadata contract: the engine
 *  chip (`index · N matches`) and the fallback-note line in Message.tsx
 *  render from this parsing. The output shapes mirror the Rust tools'
 *  summary lines (search.rs / search_read.rs). */
describe("searchResultInfo", () => {
  it("parses an index-engine summary", () => {
    const out = "40 matches in 40 files (engine: index)\n\nf0.rs:1: fn needle_fn() {}";
    expect(searchResultInfo(out)).toEqual({
      engine: "index",
      matches: 40,
      files: 40,
      note: null,
    });
  });

  it("parses a walk-engine summary", () => {
    const out =
      "2 matches in 1 files (searched 5, skipped 1, engine: walk)\n\na.rs:1: x\na.rs:2: x";
    expect(searchResultInfo(out)).toEqual({
      engine: "walk",
      matches: 2,
      files: 1,
      note: null,
    });
  });

  it("parses the search_read summary variant ('; reading top N')", () => {
    const out = "3 matches in 2 files (engine: index); reading top 2\n\n=== a.rs ===";
    expect(searchResultInfo(out)).toEqual({
      engine: "index",
      matches: 3,
      files: 2,
      note: null,
    });
  });

  it("parses 'no matches found' for both engines", () => {
    expect(searchResultInfo("no matches found (engine: index; 12 files indexed)")).toEqual({
      engine: "index",
      matches: 0,
      files: null,
      note: null,
    });
    expect(
      searchResultInfo("no matches found (searched 3 files, skipped 0, engine: walk)"),
    ).toEqual({
      engine: "walk",
      matches: 0,
      files: null,
      note: null,
    });
  });

  it("captures the prepended literal-fallback note", () => {
    const out =
      "note: invalid regex (unclosed group); matched literally instead\n\n1 matches in 1 files (engine: walk)\n\na.rs:1: call [invalid";
    expect(searchResultInfo(out)).toEqual({
      engine: "walk",
      matches: 1,
      files: 1,
      note: "invalid regex (unclosed group); matched literally instead",
    });
  });

  it("keeps the summary total when the cap note is appended", () => {
    const out =
      "150 matches in 3 files (engine: index)\n\na.rs:1: x\n... and more matches (narrow your pattern or use a glob filter)";
    expect(searchResultInfo(out)?.matches).toBe(150);
  });

  it("returns null when no engine marker is present (errors, legacy output)", () => {
    expect(searchResultInfo("ok")).toBeNull();
    expect(searchResultInfo("invalid arguments: missing pattern")).toBeNull();
  });
});

/** Pins the failed-tool-result → one-line summary contract: the ToolCard
 *  renders this below the header so the user sees what went wrong without
 *  expanding the card. */
describe("toolErrorSummary", () => {
  it("returns a single-line error verbatim", () => {
    expect(toolErrorSummary("failed to read 'x.txt': No such file or directory")).toBe(
      "failed to read 'x.txt': No such file or directory",
    );
  });

  it("takes only the first non-empty line of multi-line output", () => {
    const out = "Error: invalid arguments: missing field `path`\n\nUsage: file_read {\"path\": ...}";
    expect(toolErrorSummary(out)).toBe("Error: invalid arguments: missing field `path`");
  });

  it("skips leading blank/whitespace lines", () => {
    const out = "\n   \npath validation failed: outside root\nsecond line";
    expect(toolErrorSummary(out)).toBe("path validation failed: outside root");
  });

  it("truncates a long line to 120 chars with an ellipsis", () => {
    const long = "x".repeat(300);
    const summary = toolErrorSummary(long);
    expect(summary.length).toBe(120);
    expect(summary.endsWith("…")).toBe(true);
    expect(summary.slice(0, 119)).toBe("x".repeat(119));
  });

  it("does not truncate a line that is exactly 120 chars", () => {
    const exact = "y".repeat(120);
    expect(toolErrorSummary(exact)).toBe(exact);
  });

  it("returns empty string for empty/whitespace-only input", () => {
    expect(toolErrorSummary("")).toBe("");
    expect(toolErrorSummary("   \n\t\n  ")).toBe("");
  });
});

/**
 * Regression (backlog 2a03710d): the ToolCard header rendered one chip per
 * path PER CALL with no dedup — reading the same file in two grouped
 * read_files calls showed the basename twice. `dedupePaths` +
 * `buildPathChips` collect paths across all calls and dedupe by normalized
 * path so each file appears at most once.
 */
describe("dedupePaths", () => {
  it("drops duplicate paths, keeping the first occurrence", () => {
    expect(dedupePaths(["a.rs", "a.rs", "b.rs"])).toEqual(["a.rs", "b.rs"]);
    expect(dedupePaths(["a.rs"])).toEqual(["a.rs"]);
    expect(dedupePaths([])).toEqual([]);
  });

  it("normalizes back/forward slashes and case before deduping", () => {
    // Windows vs POSIX path separators + case-insensitive filesystems: the
    // same file must dedupe regardless of separator or casing.
    expect(dedupePaths(["src/main.rs", "src\\main.rs"])).toEqual(["src/main.rs"]);
    expect(dedupePaths(["Src/Main.rs", "src/main.rs"])).toEqual(["Src/Main.rs"]);
  });

  it("keeps distinct files that merely share a basename", () => {
    // Two DIFFERENT files with the same basename must both survive — they are
    // not duplicates, just same-named.
    expect(dedupePaths(["src/a/main.rs", "src/b/main.rs"])).toEqual([
      "src/a/main.rs",
      "src/b/main.rs",
    ]);
  });
});

describe("buildPathChips", () => {
  it("shows each file once even when read in multiple grouped calls", () => {
    // The bug: same file read in two read_files calls → header showed the
    // basename twice. After the fix, one chip.
    const calls = [
      { id: "c1", args: JSON.stringify({ path: "src/main.rs" }) },
      { id: "c2", args: JSON.stringify({ path: "src/main.rs" }) },
    ];
    const chips = buildPathChips(calls, "read_files");
    expect(chips).toEqual([
      { key: expect.any(String), text: "main.rs", path: "src/main.rs", line: 1 },
    ]);
  });

  it("dedupes within a single batched read_files call too", () => {
    const calls = [
      {
        id: "c1",
        args: JSON.stringify({ files: [{ path: "a.rs" }, { path: "a.rs" }, { path: "b.rs" }] }),
      },
    ];
    const chips = buildPathChips(calls, "read_files");
    expect(chips).toEqual([
      { key: expect.any(String), text: "a.rs", path: "a.rs", line: 1 },
      { key: expect.any(String), text: "b.rs", path: "b.rs", line: 1 },
    ]);
  });

  it("caps at 3 chips with a +N overflow from the deduped list", () => {
    // 5 specs but a.rs is a dup → 4 distinct → 3 shown + "+1".
    const calls = [
      {
        id: "c1",
        args: JSON.stringify({
          files: [
            { path: "a.rs" },
            { path: "b.rs" },
            { path: "c.rs" },
            { path: "d.rs" },
            { path: "a.rs" },
          ],
        }),
      },
    ];
    const chips = buildPathChips(calls, "read_files");
    expect(chips).toEqual([
      { key: expect.any(String), text: "a.rs", path: "a.rs", line: 1 },
      { key: expect.any(String), text: "b.rs", path: "b.rs", line: 1 },
      { key: expect.any(String), text: "c.rs", path: "c.rs", line: 1 },
      { key: expect.any(String), text: "+1", path: null, line: null },
    ]);
  });

  it("returns no chips for label-only tools (git, shell)", () => {
    // git/shell carry no file path — buildPathChips yields nothing (the label
    // chip is rendered separately by the ToolCard).
    expect(buildPathChips([{ id: "c1", args: '{"subcommand":"status"}' }], "git")).toEqual([]);
    expect(buildPathChips([{ id: "c1", args: '{"purpose":"running tests"}' }], "shell")).toEqual(
      [],
    );
  });
});

/**
 * Regression (2026-12 user report): `dedupePaths` dedupes by FULL normalized
 * path, so two DISTINCT files sharing a basename still produced two header
 * chips with identical display text — "openai.rs  openai.rs". Chips must be
 * re-qualified with parent-directory segments until every rendered name is
 * unique (full links/titles untouched).
 */
describe("buildPathChips — unique display names", () => {
  it("qualifies same-basename chips across grouped read_files calls", () => {
    // Both review flows touch an openai.rs — the card must not print the bare
    // basename twice.
    const calls = [
      { id: "c1", args: JSON.stringify({ path: "src/provider/openai.rs" }) },
      { id: "c2", args: JSON.stringify({ path: "src-tauri/src/openai.rs" }) },
    ];
    const chips = buildPathChips(calls, "file_edit");
    expect(chips.map((c) => c.text)).toEqual(["provider/openai.rs", "src/openai.rs"]);
    // The links keep their original full paths.
    expect(chips.map((c) => c.path)).toEqual(["src/provider/openai.rs", "src-tauri/src/openai.rs"]);
  });

  it("walks up segments until a deeper collision resolves", () => {
    // Last-2-segment qualification still collides here ("y/main.rs" twice);
    // the extension must continue until the texts differ.
    const calls = [
      { id: "c1", args: JSON.stringify({ files: [{ path: "x/y/main.rs" }] }) },
      { id: "c2", args: JSON.stringify({ files: [{ path: "z/y/main.rs" }] }) },
    ];
    const chips = buildPathChips(calls, "read_files");
    expect(chips.map((c) => c.text)).toEqual(["x/y/main.rs", "z/y/main.rs"]);
  });

  it("leaves non-colliding chips unqualified", () => {
    const calls = [
      {
        id: "c1",
        args: JSON.stringify({
          files: [{ path: "docs/readme.md" }, { path: "src/util.ts" }, { path: "app/util.ts" }],
        }),
      },
    ];
    const chips = buildPathChips(calls, "read_files");
    expect(chips.map((c) => c.text)).toEqual(["readme.md", "src/util.ts", "app/util.ts"]);
  });

  it("qualifies through Windows backslash paths identically", () => {
    const calls = [
      { id: "c1", args: JSON.stringify({ files: [{ path: "C:\\repo\\a\\m.rs" }] }) },
      { id: "c2", args: JSON.stringify({ files: [{ path: "D:\\other\\b\\m.rs" }] }) },
    ];
    const chips = buildPathChips(calls, "read_files");
    expect(chips.map((c) => c.text)).toEqual(["a/m.rs", "b/m.rs"]);
  });

  it("terminates on same-segment-sequence paths with a leading slash", () => {
    // "/src/main.rs" and "src/main.rs" survive dedupePaths (normalizePath
    // keeps the leading slash) yet share one segment sequence — qualification
    // would never diverge. Regression: the walk must be bounded and fall back
    // to the raw paths instead of looping forever (UI hang).
    const calls = [
      { id: "c1", args: JSON.stringify({ path: "/src/main.rs" }) },
      { id: "c2", args: JSON.stringify({ path: "src/main.rs" }) },
    ];
    const chips = buildPathChips(calls, "read_files");
    expect(chips.map((c) => c.text)).toEqual(["/src/main.rs", "src/main.rs"]);
    expect(chips.map((c) => c.path)).toEqual(["/src/main.rs", "src/main.rs"]);
  });

  it("terminates on same-segment-sequence paths with an interior double slash", () => {
    const calls = [
      { id: "c1", args: JSON.stringify({ path: "src//util.ts" }) },
      { id: "c2", args: JSON.stringify({ path: "src/util.ts" }) },
    ];
    const chips = buildPathChips(calls, "read_files");
    expect(chips.map((c) => c.text)).toEqual(["src//util.ts", "src/util.ts"]);
    expect(chips.map((c) => c.path)).toEqual(["src//util.ts", "src/util.ts"]);
  });

  it("keeps overflow accounting unchanged after qualification", () => {
    const calls = [
      {
        id: "c1",
        args: JSON.stringify({
          files: [
            { path: "x/y/main.rs" },
            { path: "z/y/main.rs" },
            { path: "b.rs" },
            { path: "c.rs" },
          ],
        }),
      },
    ];
    const chips = buildPathChips(calls, "read_files");
    expect(chips).toEqual([
      { key: expect.any(String), text: "x/y/main.rs", path: "x/y/main.rs", line: 1 },
      { key: expect.any(String), text: "z/y/main.rs", path: "z/y/main.rs", line: 1 },
      { key: expect.any(String), text: "b.rs", path: "b.rs", line: 1 },
      { key: expect.any(String), text: "+1", path: null, line: null },
    ]);
  });
});
/**
 * Unit tests for `browserResultInfo` — the trailing result chip for browser
 * tool calls (user report 2026-09-09: "browser_navigate https://example.com
 * … should show what it does"). Navigate shows the landed URL; snapshot shows
 * the page title; screenshot shows the PNG filename; other tools → null.
 */
describe("isBrowserToolName", () => {
  it("true for both browser families", () => {
    expect(isBrowserToolName("browser_navigate")).toBe(true);
    expect(isBrowserToolName("browser_click")).toBe(true);
    expect(isBrowserToolName("browser_screenshot")).toBe(true);
    expect(isBrowserToolName("offscreen_browser_navigate")).toBe(true);
    expect(isBrowserToolName("offscreen_browser_screenshot")).toBe(true);
  });

  it("false for non-browser tools", () => {
    expect(isBrowserToolName("shell")).toBe(false);
    expect(isBrowserToolName("search")).toBe(false);
    expect(isBrowserToolName("file_read")).toBe(false);
    expect(isBrowserToolName("browser")).toBe(false);
  });
});

describe("shellCallFromArgs", () => {
  it("parses command, purpose, and cwd", () => {
    expect(
      shellCallFromArgs('{"command":"cargo test","purpose":"run tests","cwd":"frontend"}'),
    ).toEqual({ command: "cargo test", purpose: "run tests", cwd: "frontend" });
  });

  it("includes only present, non-empty optional fields", () => {
    expect(shellCallFromArgs('{"command":"ls"}')).toEqual({ command: "ls" });
    expect(shellCallFromArgs('{"command":"ls","purpose":"","cwd":"  "}')).toEqual({
      command: "ls",
    });
  });

  it("returns null for missing/empty/non-string command", () => {
    expect(shellCallFromArgs('{"purpose":"x"}')).toBeNull();
    expect(shellCallFromArgs('{"command":""}')).toBeNull();
    expect(shellCallFromArgs('{"command":42}')).toBeNull();
    expect(shellCallFromArgs("[]")).toBeNull();
    expect(shellCallFromArgs("null")).toBeNull();
  });

  it("returns null for unparseable JSON", () => {
    expect(shellCallFromArgs("not json")).toBeNull();
  });
});

describe("parseShellOutput", () => {
  it("plain stdout without markers", () => {
    expect(parseShellOutput("hello\nworld")).toEqual({
      stdout: "hello\nworld",
      stderr: null,
      exitCode: null,
    });
  });

  it("splits stderr on the first marker", () => {
    expect(parseShellOutput("out1\nout2\n[stderr]\nerr text")).toEqual({
      stdout: "out1\nout2",
      stderr: "err text",
      exitCode: null,
    });
  });

  it("extracts the exit-code trailer as the final line", () => {
    expect(parseShellOutput("out\n[exit code: 0]")).toEqual({
      stdout: "out",
      stderr: null,
      exitCode: 0,
    });
    expect(parseShellOutput("out\n[exit code: 42]")).toEqual({
      stdout: "out",
      stderr: null,
      exitCode: 42,
    });
    expect(parseShellOutput("out\r\n[exit code: -1]")).toEqual({
      stdout: "out",
      stderr: null,
      exitCode: -1,
    });
  });

  it("keeps mid-text '[exit code: N]' occurrences in stdout", () => {
    expect(parseShellOutput("found [exit code: 42] in a doc\nmore")).toEqual({
      stdout: "found [exit code: 42] in a doc\nmore",
      stderr: null,
      exitCode: null,
    });
  });

  it("does not extract a trailer-shaped line that carries extra text", () => {
    expect(parseShellOutput("out\nexit code: 0 was nice")).toEqual({
      stdout: "out\nexit code: 0 was nice",
      stderr: null,
      exitCode: null,
    });
  });

  it("splits stderr before extracting the trailer (full shell.rs contract shape)", () => {
    expect(parseShellOutput("out\n[stderr]\nerr\n[exit code: 3]")).toEqual({
      stdout: "out",
      stderr: "err",
      exitCode: 3,
    });
  });

  it("preserves output-filter notes riding at the top of stdout", () => {
    const parts = parseShellOutput("TIP: use the search tool\nreal output\n[exit code: 0]");
    expect(parts.stdout).toBe("TIP: use the search tool\nreal output");
    expect(parts.stderr).toBeNull();
    expect(parts.exitCode).toBe(0);
  });

  it("empty output", () => {
    expect(parseShellOutput("")).toEqual({ stdout: "", stderr: null, exitCode: null });
  });
});

describe("browserResultInfo", () => {
  it("navigate: extracts the landed URL", () => {
    expect(
      browserResultInfo("browser_navigate", "navigated the Browser tab to https://example.com"),
    ).toBe("→ https://example.com");
  });

  it("navigate failed → null", () => {
    expect(
      browserResultInfo("browser_navigate", "browser_navigate failed: no page target"),
    ).toBeNull();
  });

  it("snapshot: extracts the page title", () => {
    expect(
      browserResultInfo("browser_snapshot", "<html><head><title>Example Domain</title></head></html>"),
    ).toBe("Example Domain");
  });

  it("screenshot: extracts the PNG filename", () => {
    expect(
      browserResultInfo(
        "browser_screenshot",
        "captured screenshot → .coding/browser/screenshots/browser-123.png",
      ),
    ).toBe("browser-123.png");
  });

  it("click/type/eval + unknown tools → null", () => {
    expect(browserResultInfo("browser_click", "clicked '#x'")).toBeNull();
    expect(browserResultInfo("browser_type", "typed into '#q'")).toBeNull();
    expect(browserResultInfo("browser_eval", '"42"')).toBeNull();
    expect(browserResultInfo("file_edit", "navigated the Browser tab to https://x.com")).toBeNull();
  });
});

/**
 * webFetchLabel — the fetched URL surfaced on the web_fetch ToolCard header
 * (backlog 69cf7e9c — "web_fetch should be showing the url it fetches in the
 * main window"), mirroring browser_navigate's url chip (user report
 * 2026-09-09).
 */
describe("webFetchLabel", () => {
  it("returns the url for web_fetch calls", () => {
    expect(
      webFetchLabel("web_fetch", '{"url":"https://example.com/docs","max_length":5000}'),
    ).toBe("https://example.com/docs");
  });

  it("truncates a long url with an ellipsis (the header row must not stretch)", () => {
    const url = "https://example.com/a/very/long/path/that/goes/on/and/on/and/on/forever";
    const label = webFetchLabel("web_fetch", JSON.stringify({ url }));
    expect(label).toBe(`${url.slice(0, 60)}…`);
  });

  it("returns null for a missing/blank url, malformed args, or other tools", () => {
    expect(webFetchLabel("web_fetch", '{"max_length":5000}')).toBeNull();
    expect(webFetchLabel("web_fetch", '{"url":"   "}')).toBeNull();
    expect(webFetchLabel("web_fetch", "{broken json")).toBeNull();
    // Even with a url in the args, only web_fetch gets the label — the
    // browser family renders its own via browserArgLabel.
    expect(webFetchLabel("browser_navigate", '{"url":"https://example.com"}')).toBeNull();
    expect(webFetchLabel("file_read", '{"url":"https://example.com"}')).toBeNull();
  });
});

/**
 * webFetchUrl — the FULL untruncated http(s) URL behind the clickable
 * web_fetch chip (plan 22001355): the chip's label is webFetchLabel's
 * 60-char truncation, but the click must open the exact page that was
 * fetched — and only http(s), so an LLM-controlled arg can never turn the
 * chip into a launcher for a non-web target.
 */
describe("webFetchUrl", () => {
  it("returns the full url untruncated while the label truncates at 60", () => {
    const url = "https://example.com/a/very/long/path/that/goes/on/and/on/and/on/forever";
    const args = JSON.stringify({ url });
    expect(webFetchLabel("web_fetch", args)).toBe(`${url.slice(0, 60)}…`);
    expect(webFetchUrl("web_fetch", args)).toBe(url);
  });

  it("accepts http and https, rejects every other scheme", () => {
    // The returned url is the parsed URL's normalized href — a bare host
    // gains its trailing slash (review LOW 3).
    expect(webFetchUrl("web_fetch", '{"url":"http://example.com"}')).toBe("http://example.com/");
    expect(webFetchUrl("web_fetch", '{"url":"https://example.com/docs"}')).toBe(
      "https://example.com/docs",
    );
    // Surrounding whitespace is trimmed off the returned url.
    expect(webFetchUrl("web_fetch", '{"url":"  https://example.com/padded  "}')).toBe(
      "https://example.com/padded",
    );
    // Odd-but-parseable http(s) forms normalize to their href so the OS
    // opener receives exactly what a browser would resolve (review LOW 3).
    expect(webFetchUrl("web_fetch", '{"url":"http:example.com"}')).toBe("http://example.com/");
    expect(webFetchUrl("web_fetch", JSON.stringify({ url: "https:\\example.com" }))).toBe(
      "https://example.com/",
    );
    expect(webFetchUrl("web_fetch", '{"url":"file:///C:/Windows/system.ini"}')).toBeNull();
    expect(webFetchUrl("web_fetch", '{"url":"ftp://example.com/file"}')).toBeNull();
    expect(webFetchUrl("web_fetch", '{"url":"mailto:user@example.com"}')).toBeNull();
    expect(webFetchUrl("web_fetch", '{"url":"javascript:alert(1)"}')).toBeNull();
    expect(webFetchUrl("web_fetch", '{"url":"not a url"}')).toBeNull();
  });

  it("returns null for a missing/blank url, malformed args, or other tools", () => {
    expect(webFetchUrl("web_fetch", '{"max_length":5000}')).toBeNull();
    expect(webFetchUrl("web_fetch", '{"url":"   "}')).toBeNull();
    expect(webFetchUrl("web_fetch", "{broken json")).toBeNull();
    // Even with a url in the args, only web_fetch gets the link — the
    // browser family renders its own via browserArgLabel.
    expect(webFetchUrl("browser_navigate", '{"url":"https://example.com"}')).toBeNull();
    expect(webFetchUrl("file_read", '{"url":"https://example.com"}')).toBeNull();
  });
});

/**
 * Source-contract tests for the expandable memory activity card (backlog
 * 41f39672 — "12 matched, alas no expanding to see the 12"). Node-env suite
 * (no DOM renderer), so the card's wiring is pinned directly against the
 * component source — the same pattern the CSS tests use on globals.css: a
 * regression that un-expands the memory_search card, drops the per-hit
 * score, or breaks the parser→entry→card chain fails loudly here.
 */
describe("expandable memory_search card (source contract)", () => {
  const readSrc = (rel: string): string =>
    readFileSync(new URL(rel, import.meta.url), "utf8");

  const messageTsx = () =>
    readSrc("../components/chat/Message.tsx");
  const reducer = () =>
    readSrc("../hooks/agentEventReducer.ts");

  it("MemoryEntryCard is expandable whenever parsed hits exist (no auto-recall-only gate)", () => {
    const src = messageTsx();
    // The gate must key off the hits list itself, not the entry name — the
    // old `entry.name === "auto-recall" && hits.length > 0` shape would
    // leave memory_search cards non-expandable.
    expect(src).toContain("const expandable = hits.length > 0;");
    expect(src).not.toContain('entry.name === "auto-recall" && hits.length > 0');
    // Chevron + expanded list still wired (the shared expand interaction).
    expect(src).toContain("ChevronRight");
    expect(src).toContain("ChevronDown");
  });

  it("the expanded list renders tier + title + score per hit", () => {
    const src = messageTsx();
    expect(src).toContain("h.tier");
    expect(src).toContain("h.title");
    expect(src).toContain("score");
  });

  it("the reducer attaches parsed hits only for real hit lists", () => {
    const src = reducer();
    // memory_search results finalize with the parsed hits (backlog ask);
    // empty lists stay undefined so other memory tools remain
    // non-expandable.
    expect(src).toContain("hits: hits?.length ? hits : undefined");
    expect(src).toContain("while ((hmExec = hitRe.exec(out)) !== null)");
  });
});

/**
 * Source-contract tests for the frameless + normal-size card treatment
 * (backlog 900fe8d8): the memory, vision, and skill activity cards drop
 * their border/bg frames and text downscales and match the ToolCard's
 * frameless template; card body text inherits 1em of the chat content
 * (--app-font-size). Em/inherited classes scale with the setting by
 * construction, so the no-rem audit below IS the two-font-size guarantee
 * (rem-based utilities would NOT scale). Node-env suite (no DOM renderer),
 * pinned against the component source like the block above.
 */
describe("frameless + normal-size cards (backlog 900fe8d8)", () => {
  const readSrc = (rel: string): string =>
    readFileSync(new URL(rel, import.meta.url), "utf8");

  const messageTsx = () => readSrc("../components/chat/Message.tsx");

  /** The source of a top-level component fn, from `function NAME(` to the
   *  next top-level `function ` (a following fn's doc comment is included —
   *  harmless: the assertions use exact utility strings). */
  const fnSrc = (src: string, name: string): string => {
    const start = src.indexOf(`function ${name}(`);
    if (start === -1) throw new Error(`function ${name} not found in Message.tsx`);
    const next = src.indexOf("\nfunction ", start + 1);
    return next === -1 ? src.slice(start) : src.slice(start, next);
  };

  /** The source of a MessageImpl render case block: the LAST `case "KIND":`
   *  (the sameEntry equality switch carries an earlier one) to the next case at the same depth. */
  const caseSrc = (src: string, kind: string): string => {
    const start = src.lastIndexOf(`case "${kind}":`);
    if (start === -1) throw new Error(`case "${kind}" not found in Message.tsx`);
    const next = src.indexOf("\n    case ", start + 1);
    return next === -1 ? src.slice(start) : src.slice(start, next);
  };

  // Rem-based utilities must never appear in a card: they ignore
  // --app-font-size (SPEC 4c2f16ba — spacing em-based; backlog 900fe8d8
  // extends it to font sizes: the body inherits, labels use em).
  const REM_UTILITIES = [
    "text-xs",
    "text-sm",
    "text-[0.875em]",
    "text-[0.65rem]",
    "gap-2",
    "px-3",
    "py-1.5",
    "h-3 ",
    "w-3 ",
    "mt-1.5",
    "pt-1.5",
    "px-1.5",
    "py-0.5",
    "space-y-0.5",
    "space-y-1.5",
    "max-h-64",
  ];

  it("memory, vision, and skill activity cards are frameless with em-based spacing", () => {
    const src = messageTsx();
    const scopes: [string, string][] = [
      ["MemoryEntryCard", fnSrc(src, "MemoryEntryCard")],
      ["VisionEntryCard", fnSrc(src, "VisionEntryCard")],
      ['case "skill"', caseSrc(src, "skill")],
      ["ToolCard", fnSrc(src, "ToolCard")],
    ];
    for (const [name, body] of scopes) {
      // Frameless (DECISION 344bc076): the frame classes are gone — the
      // tier badges' bg-violet-900/50 chip is a label treatment, not a
      // card frame.
      expect(`${name}: ${body}`).not.toContain("rounded-lg");
      for (const frame of [
        "border-violet-600/30",
        "bg-violet-950/20",
        "border-border",
        "bg-bg-tertiary/50",
        "border-t ",
      ]) {
        expect(`${name}: ${body}`).not.toContain(frame);
      }
      // The frameless template outer.
      expect(`${name}: ${body}`).toContain("py-[0.125em]");
      // Em-based only: no rem utilities anywhere in the card.
      for (const rem of REM_UTILITIES) {
        expect(`${name}: ${body}`).not.toContain(rem);
      }
    }
  });

  it("the ToolCard body inherits the chat content size (labels keep the smaller em treatment)", () => {
    const src = messageTsx();
    const tool = fnSrc(src, "ToolCard");
    // The header downscale is gone — the card line renders at 1em of the
    // chat content (--app-font-size).
    expect(tool).not.toContain("text-[0.875em]");
    // The status/running indicators keep the smaller em-based label
    // treatment (scales with --app-font-size).
    expect(tool).toContain("text-[0.75em]");
    // No rem utilities (the images row's gap-2 is gone).
    for (const rem of REM_UTILITIES) {
      expect(tool).not.toContain(rem);
    }
  });

  it("the expanded CallDetail body stays em-based (labels + code at the smaller em treatment)", () => {
    const src = messageTsx();
    const detail = fnSrc(src, "CallDetail");
    for (const rem of REM_UTILITIES) {
      expect(detail).not.toContain(rem);
    }
    // The labels/pre blocks keep the em-based code treatment (mirrors the
    // assistant prose's own CodeBlock).
    expect(detail).toContain("text-[0.75em]");
    // The diff viewer gets an em-based scroll cap on the card path (the
    // shared default max-h-64 stays for non-card consumers).
    expect(detail).toContain('maxHeightClass="max-h-[24em]"');
  });

  it("affordances survive the conversion", () => {
    const src = messageTsx();
    const memory = fnSrc(src, "MemoryEntryCard");
    expect(memory).toContain('role="button"');
    expect(memory).toContain("ChevronRight");
    expect(memory).toContain("ChevronDown");
    expect(memory).toContain("h.tier");
    expect(memory).toContain("h.title");
    expect(memory).toContain("score");
    // The tier badges keep their label treatment — em-based now.
    expect(memory).toContain("text-[0.65em]");
    expect(memory).not.toContain("text-[0.65rem]");
    const vision = fnSrc(src, "VisionEntryCard");
    expect(vision).toContain('role="button"');
    expect(vision).toContain("ChevronRight");
    expect(vision).toContain("ChevronDown");
  });
});

/**
 * fileEditDiff — the expanded file_edit card renders the Rust-side unified
 * diff carried in the tool result's `data` payload instead of the raw
 * old_string/new_string args JSON (backlog cb3461fe).
 */
describe("fileEditDiff", () => {
  it("returns the diff from a successful result's data payload", () => {
    const diff = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n";
    expect(
      fileEditDiff({ success: true, output: "edited src/main.rs", data: { diff } }),
    ).toBe(diff);
  });

  it("null for a failed edit (the error text is what matters there)", () => {
    expect(fileEditDiff({ success: false, output: "not found", data: { diff: "x" } })).toBeNull();
  });

  it("null when the payload is absent, non-object, or diff is not a non-blank string", () => {
    expect(fileEditDiff({ success: true, output: "edited" })).toBeNull();
    expect(fileEditDiff({ success: true, output: "edited", data: null })).toBeNull();
    expect(fileEditDiff({ success: true, output: "edited", data: "diff" })).toBeNull();
    expect(fileEditDiff({ success: true, output: "edited", data: { diff: 42 } })).toBeNull();
    expect(fileEditDiff({ success: true, output: "edited", data: { diff: "   " } })).toBeNull();
  });

  it("null for a still-running call (no result yet)", () => {
    expect(fileEditDiff(null)).toBeNull();
  });
});

/**
 * parseReadFilesSections — the expanded read_files card shows just WHAT was
 * read (per-file line ranges parsed from the result's section headers)
 * instead of dumping every file's full numbered content (backlog cb3461fe).
 */
describe("parseReadFilesSections", () => {
  it("parses each file's actual line range from the section headers", () => {
    const out = [
      "=== src/main.rs (lines 1-500 of 1354) ===",
      "     1: fn main() {}",
      "",
      "=== src/lib.rs (lines 40-90 of 120) ===",
      "    40: pub fn x() {}",
    ].join("\n");
    expect(parseReadFilesSections(out)).toEqual([
      { kind: "range", path: "src/main.rs", first: 1, last: 500, total: 1354 },
      { kind: "range", path: "src/lib.rs", first: 40, last: 90, total: 120 },
    ]);
  });

  it("captures error and empty-range sections, with the error's first line as the note", () => {
    const out = [
      "=== src/gone.rs (error) ===",
      "read failed: no such file",
      "more detail",
      "",
      "=== src/empty.rs (empty range) ===",
    ].join("\n");
    expect(parseReadFilesSections(out)).toEqual([
      { kind: "note", path: "src/gone.rs", note: "read failed: no such file" },
      { kind: "note", path: "src/empty.rs", note: "empty range" },
    ]);
  });

  it("ignores the SYMBOL NUDGE prefix and truncation tails (no headers there)", () => {
    const out = [
      "SYMBOL NUDGE: pattern has no regex metacharacters — literal:true would use the content-index engine",
      "",
      "=== src/main.rs (lines 1-10 of 10) ===",
      "     1: x",
      "",
      "... (truncated: total output exceeded size limit)",
    ].join("\n");
    expect(parseReadFilesSections(out)).toEqual([
      { kind: "range", path: "src/main.rs", first: 1, last: 10, total: 10 },
    ]);
  });

  it("returns [] when no section header matches (failed call's error output)", () => {
    expect(parseReadFilesSections("files array is empty")).toEqual([]);
    expect(parseReadFilesSections("")).toEqual([]);
  });
});

/**
 * argPathLines — read_files chips carry the line the read started at so the
 * link opens the file there (backlog cb3461fe).
 */
describe("argPathLines", () => {
  it("pairs each read_files spec with its start_line (defaulting to 1)", () => {
    const args = JSON.stringify({
      files: [
        { path: "src/main.rs" },
        { path: "src/lib.rs", start_line: 40 },
        { path: "src/util.ts", start_line: 0 }, // < 1 → the default
      ],
    });
    expect(argPathLines(args, "read_files")).toEqual([
      { path: "src/main.rs", line: 1 },
      { path: "src/lib.rs", line: 40 },
      { path: "src/util.ts", line: 1 },
    ]);
  });

  it("handles the single-file shorthand (top-level path + start_line)", () => {
    expect(argPathLines('{"path":"src/main.rs","start_line":42}', "read_files")).toEqual([
      { path: "src/main.rs", line: 42 },
    ]);
    expect(argPathLines('{"path":"src/main.rs"}', "read_files")).toEqual([
      { path: "src/main.rs", line: 1 },
    ]);
  });

  it("non-read tools return their argPaths paths with line null", () => {
    expect(argPathLines('{"path":"src/main.rs"}', "file_edit")).toEqual([
      { path: "src/main.rs", line: null },
    ]);
  });

  it("malformed read_files JSON falls back to argPaths' salvage with line null", () => {
    expect(argPathLines('{"path": "src/main.rs"', "read_files")).toEqual([
      { path: "src/main.rs", line: null },
    ]);
  });
});

/**
 * The remaining argLabel branches — the git / read_files / git_read /
 * browser / backlog_add branches are covered by messageArgLabel.test.ts.
 * argLabel + displayName moved from Message.tsx to toolCardPaths.ts by
 * quality review LOW 2.
 */
describe("argLabel — remaining branches", () => {
  it("shell: shows the purpose field", () => {
    expect(argLabel('{"purpose":"running tests"}', "shell")).toBe("running tests");
  });

  it("shell: blank or absent purpose → null", () => {
    expect(argLabel('{"purpose":"  "}', "shell")).toBeNull();
    expect(argLabel('{"command":"ls"}', "shell")).toBeNull();
  });

  it("spawn_agent: shows the agent's display name", () => {
    expect(argLabel('{"name":"rust-core-reviewer"}', "spawn_agent")).toBe("rust-core-reviewer");
    expect(argLabel('{"task":"x"}', "spawn_agent")).toBeNull();
  });

  it("skill_start: shows the skill name", () => {
    expect(argLabel('{"skill":"merge_to_main"}', "skill_start")).toBe("merge_to_main");
    expect(argLabel('{"prompt":"x"}', "skill_start")).toBeNull();
  });

  it("skill_create: shows the skill being authored", () => {
    expect(argLabel('{"name":"deploy_checklist"}', "skill_create")).toBe("deploy_checklist");
    expect(argLabel('{"name":"  deploy_checklist  "}', "skill_create")).toBe("deploy_checklist");
    expect(argLabel('{"prompt":"x"}', "skill_create")).toBeNull();
    expect(argLabel("{bad json", "skill_create")).toBeNull();
    // skill_reload takes no arguments — its card stays bare by design.
    expect(argLabel("{}", "skill_reload")).toBeNull();
  });

  it("search / search_read: quoted pattern, optional glob", () => {
    expect(argLabel('{"pattern":"WorkflowState"}', "search")).toBe('"WorkflowState"');
    expect(argLabel('{"pattern":"WorkflowState","glob":"**/*.rs"}', "search")).toBe(
      '"WorkflowState" in **/*.rs',
    );
    expect(argLabel('{"pattern":"x","glob":"**/*.rs"}', "search_read")).toBe('"x" in **/*.rs');
    expect(argLabel('{"glob":"**/*.rs"}', "search")).toBeNull();
  });

  it("graph_* tools delegate to graphCallLabel", () => {
    expect(argLabel('{"query":"GraphView"}', "graph_search")).toBe('"GraphView"');
    expect(argLabel('{"name":"MemoryStore"}', "graph_context")).toBe("MemoryStore");
  });

  it("web_fetch delegates to webFetchLabel", () => {
    const args = '{"url":"https://example.com"}';
    expect(argLabel(args, "web_fetch")).toBe(webFetchLabel("web_fetch", args));
  });

  it("common path/file fallback shows the basename", () => {
    expect(argLabel('{"path":"src/agent/turn.rs"}', "file_edit")).toBe("turn.rs");
    expect(argLabel('{"file":"docs/README.md"}', "some_tool")).toBe("README.md");
  });

  it("malformed JSON → null", () => {
    expect(argLabel("{bad json}", "file_read")).toBeNull();
  });
});

describe("displayName", () => {
  it("maps the known tool names to friendly labels", () => {
    expect(displayName("spawn_agent")).toBe("Spawn Agent");
    expect(displayName("graph_search")).toBe("Graph Search");
    expect(displayName("graph_context")).toBe("Graph Context");
    expect(displayName("graph_impact")).toBe("Graph Impact");
    expect(displayName("graph_path")).toBe("Graph Path");
  });

  it("falls back to the raw name", () => {
    expect(displayName("shell")).toBe("shell");
    expect(displayName("read_files")).toBe("read_files");
  });
});

/**
 * Source-contract tests for the live shell output tail (backlog 7e6385b3,
 * plan 0d2c1221): while a shell call is still running, its ToolCard shows the
 * command's output as it is produced instead of leaving the user blind until
 * exit. Node-env suite (no DOM renderer), so the render site is pinned against
 * the component source — the same pattern as the card contracts above: a
 * regression that shows the tail on a FINISHED card, drops the running gate,
 * or un-caps the block fails loudly here.
 */
describe("live shell output tail (backlog 7e6385b3)", () => {
  const readSrc = (rel: string): string =>
    readFileSync(new URL(rel, import.meta.url), "utf8");

  it("liveTailPreview keeps the newest lines and caps from the END", () => {
    // Everything fits: returned verbatim (a trailing newline included, so a
    // half-printed line still reads as one).
    expect(liveTailPreview("a\nb\nc")).toBe("a\nb\nc");
    expect(liveTailPreview("done\n")).toBe("done\n");
    // More lines than fit → the LAST maxLines survive (progress, not history).
    expect(liveTailPreview("1\n2\n3\n4\n5\n6\n7\n8")).toBe("3\n4\n5\n6\n7\n8");
    // One huge line is trimmed from the FRONT — the newest chars win.
    expect(liveTailPreview("x".repeat(500))).toBe("x".repeat(400));
    expect(liveTailPreview("old\n" + "y".repeat(500))).toBe("y".repeat(400));
  });

  it("ToolCard shows the tail of the RUNNING call, under the header", () => {
    const src = readSrc("../components/chat/Message.tsx");
    // The gate keys off the running call itself (`result === null`), so a
    // result always replaces the preview; the reducer clears liveOutput on
    // tool_result as the second line of defence.
    expect(src).toContain("calls.find((c) => c.result === null && c.liveOutput)");
    expect(src).toContain('{liveTail !== "" && (');
    // Follow the newest output (a chatty command prints far more than fits).
    expect(src).toContain("el.scrollTop = el.scrollHeight");
    expect(src).toContain("max-h-[12em]");
    // Only the newest lines are rendered (the retained window is the
    // scroll-back source, not the DOM), and the block announces itself.
    expect(src).toContain("liveTailPreview(liveOutput)");
    expect(src).toContain('aria-live="polite"');
  });

  it("the reducer caps the tail and clears it the moment the result lands", () => {
    const src = readSrc("../hooks/agentEventReducer.ts");
    // 16 KiB head-drop per running call (the card renders a tail; the FULL
    // text arrives with the result).
    expect(src).toContain("export const LIVE_OUTPUT_CAP = 16 * 1024;");
    expect(src).toContain("liveOutput: appendLiveOutputTail(");
    // tool_result clears the preview — and only a call with `result === null`
    // is ever appended to (the late-chunk guard).
    expect(src).toContain("liveOutput: undefined");
    expect(src).toContain("c.id === event.tool_call_id && c.result === null");
  });

  it("the dispatcher coalesces live chunks per call instead of writing per chunk", () => {
    const src = readSrc("../hooks/useAgentEvents.ts");
    expect(src).toContain('payload.event.kind === "tool_output_delta"');
    expect(src).toContain("outputBuffers");
    // The buffers are drained on /clear like the other delta buffers, so a
    // stale chunk cannot repopulate a cleared conversation.
    expect(src).toContain("handle.outputBuffers.delete(id);");
  });
});
