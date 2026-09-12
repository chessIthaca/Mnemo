// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";

import { argLabel } from "../../lib/toolCardPaths";
import source from "./Message.tsx?raw";

/**
 * Unit tests for the git branch of the ToolCard `argLabel` (lib/toolCardPaths.ts):
 * forwarded read-query args must appear after the subcommand so a card reads
 * "git (log --stat -5)" instead of just "git (log)".
 */
describe("argLabel — git", () => {
  it("shows the subcommand alone when no args are forwarded", () => {
    expect(argLabel('{"subcommand":"status"}', "git")).toBe("status");
    expect(argLabel('{"subcommand":"log","args":[]}', "git")).toBe("log");
    expect(argLabel('{"subcommand":"commit","message":"x"}', "git")).toBe("commit");
  });

  it("shows forwarded read-query args after the subcommand", () => {
    expect(argLabel('{"subcommand":"log","args":["--stat","-5"]}', "git")).toBe("log --stat -5");
    expect(argLabel('{"subcommand":"diff","args":["main..feat","--","src/"]}', "git")).toBe(
      "diff main..feat -- src/",
    );
  });

  it("ignores non-string and blank entries in args", () => {
    expect(argLabel('{"subcommand":"log","args":["--stat",5,null," "]}', "git")).toBe(
      "log --stat",
    );
  });

  it("falls back to the action field when the subcommand is missing or blank", () => {
    // Regression (review 2026-08-22 Low 1): the git tool's forgiving API
    // accepts the verb in `action` (e.g. action="commit") or an action name
    // in `subcommand` (e.g. subcommand="delete") — such calls must still show
    // a verb in the card instead of a bare "git".
    expect(argLabel('{"action":"commit","message":"x"}', "git")).toBe("commit");
    expect(argLabel('{"action":"delete","branch":"x"}', "git")).toBe("delete");
    expect(argLabel('{"action":"log","args":["-5"]}', "git")).toBe("log -5");
    expect(argLabel('{"subcommand":" ","action":"status"}', "git")).toBe("status");
    // subcommand still wins when both are present.
    expect(argLabel('{"subcommand":"branch","action":"delete","branch":"x"}', "git")).toBe("branch");
  });

  it("returns null when the subcommand is missing", () => {
    expect(argLabel('{"args":["--stat"]}', "git")).toBeNull();
  });

  it("labels restore calls with target/source/paths", () => {
    // Bare restore (default worktree target is not shown).
    expect(argLabel('{"subcommand":"restore"}', "git")).toBe("restore");
    expect(
      argLabel('{"subcommand":"restore","target":"worktree","paths":["b.txt"]}', "git"),
    ).toBe("restore -- b.txt");
    // Paths joined after a `--` separator.
    expect(
      argLabel('{"subcommand":"restore","paths":["src/a.rs","src/b.rs"]}', "git"),
    ).toBe("restore -- src/a.rs src/b.rs");
    // Source tree-ish before the separator.
    expect(
      argLabel('{"subcommand":"restore","source":"HEAD~1","paths":["a.txt"]}', "git"),
    ).toBe("restore HEAD~1 -- a.txt");
    // Non-default target after the verb.
    expect(
      argLabel('{"subcommand":"restore","target":"staged","paths":["b.txt"]}', "git"),
    ).toBe("restore staged -- b.txt");
    expect(
      argLabel('{"subcommand":"restore","target":"Both","paths":["b.txt"]}', "git"),
    ).toBe("restore both -- b.txt");
    // The forgiving singular `path` field (backend lifts it into paths).
    expect(argLabel('{"subcommand":"restore","path":"b.txt"}', "git")).toBe("restore -- b.txt");
    // No paths at all → still a usable verb label.
    expect(argLabel('{"subcommand":"restore","target":"staged"}', "git")).toBe("restore staged");
    // Regression (review 2026-09-08 Low 1): an EMPTY paths array is treated
    // as absent by the backend normalization, which then lifts the singular
    // `path` — the chip must show the file the backend will actually
    // restore, not a bare "restore".
    expect(argLabel('{"subcommand":"restore","paths":[],"path":"b.txt"}', "git")).toBe(
      "restore -- b.txt",
    );
    // But a non-empty paths array wins and `path` is ignored (the backend
    // lifts `path` only when no non-empty array exists) — mirroring that,
    // no fall-through to `path` here.
    expect(argLabel('{"subcommand":"restore","paths":["a.txt"],"path":"b.txt"}', "git")).toBe(
      "restore -- a.txt",
    );
  });
});

/**
 * Unit tests for the read_files branch of `argLabel` (lib/toolCardPaths.ts): the Rust
 * tool accepts a single-file top-level `path` shorthand (absorbed from the old
 * file_read tool) in addition to the `files` array. Both shapes must show the
 * file name in the card header instead of a bare "read_files".
 */
describe("argLabel — read_files", () => {
  it("shows basenames for the files array shape", () => {
    const args = JSON.stringify({ files: [{ path: "src/a.rs" }, { path: "b.rs" }] });
    expect(argLabel(args, "read_files")).toBe("a.rs, b.rs");
  });

  it("caps at 3 names with +N overflow", () => {
    const args = JSON.stringify({
      files: [{ path: "a.rs" }, { path: "b.rs" }, { path: "c.rs" }, { path: "d.rs" }, { path: "e.rs" }],
    });
    expect(argLabel(args, "read_files")).toBe("a.rs, b.rs, c.rs +2");
  });

  it("falls back to the top-level path for the single-file shorthand", () => {
    // The Rust tool accepts {"path": "..."} (no files array); the card must
    // still show the file name instead of a bare "read_files".
    expect(argLabel('{"path":"src/main.rs"}', "read_files")).toBe("main.rs");
  });

  it("files array wins over a top-level path when both present", () => {
    // The shorthand only applies when there is no batch — an explicit files
    // array takes precedence (mirrors the Rust tool).
    const args = JSON.stringify({ path: "ignored.rs", files: [{ path: "a.rs" }] });
    expect(argLabel(args, "read_files")).toBe("a.rs");
  });

  it("returns null when files is empty or has no valid paths", () => {
    expect(argLabel('{"files":[]}', "read_files")).toBeNull();
    expect(argLabel('{"files":[{"foo":"bar"}]}', "read_files")).toBeNull();
  });

  it("returns null when neither files nor path is present", () => {
    expect(argLabel("{}", "read_files")).toBeNull();
  });
});

/**
 * Unit tests for the git_read branch of `argLabel` (lib/toolCardPaths.ts): the op
 * (diff | log | show) is the meaningful part of a read-only git query — the
 * card reads "git_read (log -8)" / "git_read (show d31b606)" instead of a
 * bare "git_read" (user report 2026-08-25: "git read should show some
 * information").
 */
describe("argLabel — git_read", () => {
  it("shows the op alone when no context fields are set", () => {
    expect(argLabel('{"op":"diff"}', "git_read")).toBe("diff");
    expect(argLabel('{"op":"log"}', "git_read")).toBe("log");
  });

  it("log: appends the limit and the path filter", () => {
    expect(argLabel('{"op":"log","limit":8}', "git_read")).toBe("log -8");
    expect(argLabel('{"op":"log","limit":8,"path":"src/lib"}', "git_read")).toBe(
      "log -8 -- src/lib",
    );
    expect(argLabel('{"op":"log","path":"src/lib"}', "git_read")).toBe("log -- src/lib");
  });

  it("show: appends the commit, shortened to 7 chars when long", () => {
    expect(
      argLabel('{"op":"show","commit":"d31b606abc123def456"}', "git_read"),
    ).toBe("show d31b606");
    expect(argLabel('{"op":"show","commit":"cab7592"}', "git_read")).toBe("show cab7592");
  });

  it("diff: appends the path filter when present", () => {
    expect(argLabel('{"op":"diff","path":"src/"}', "git_read")).toBe("diff src/");
  });

  it("returns null when the op is missing or malformed JSON", () => {
    expect(argLabel('{"limit":8}', "git_read")).toBeNull();
    expect(argLabel("{bad json}", "git_read")).toBeNull();
  });
});
/**
 * Unit tests for the browser branches of `argLabel` (lib/toolCardPaths.ts): the URL /
 * selector / expression is the meaningful part of a browser tool call — the
 * card reads "browser_navigate (example.com)" or "browser_click (#submit)"
 * instead of a bare tool name with raw JSON hidden behind expand (user
 * report 2026-09-09: "none of the browser commands show a nice summary").
 * Covers BOTH families — the live-tab `browser_*` tools and the
 * headless `offscreen_browser_*` tools.
 */
describe("argLabel — browser", () => {
  it("navigate: shows the target URL (browser_* + offscreen_*)", () => {
    expect(
      argLabel('{"url":"https://example.com"}', "browser_navigate"),
    ).toBe("https://example.com");
    expect(
      argLabel('{"url":"http://localhost:3000"}', "offscreen_browser_navigate"),
    ).toBe("http://localhost:3000");
  });

  it("click: shows the selector", () => {
    expect(argLabel('{"selector":"#submit"}', "browser_click")).toBe("#submit");
    expect(
      argLabel('{"selector":"a.nav-link"}', "offscreen_browser_click"),
    ).toBe("a.nav-link");
  });

  it("type: shows the selector plus the (truncated) text", () => {
    expect(
      argLabel('{"selector":"#q","text":"hello world"}', "browser_type"),
    ).toBe('#q ← "hello world"');
    expect(
      argLabel('{"selector":"#q","text":"' + "x".repeat(40) + '"}', "browser_type"),
    ).toBe(`#q ← "${"x".repeat(35)}…"`);
  });

  it("eval: shows the expression truncated", () => {
    expect(
      argLabel('{"expression":"location.href"}', "browser_eval"),
    ).toBe("location.href");
    expect(
      argLabel('{"expression":"' + "y".repeat(60) + '"}', "offscreen_browser_eval"),
    ).toBe(`${"y".repeat(45)}…`);
  });

  it("screenshot/snapshot: no meaningful arg label (image/result chips carry it)", () => {
    expect(argLabel("{}", "browser_screenshot")).toBeNull();
    expect(argLabel("{}", "browser_snapshot")).toBeNull();
  });

  it("unparseable args stay null (no crash)", () => {
    expect(argLabel("{bad json", "browser_navigate")).toBeNull();
  });
});

/**
 * Unit tests for the backlog_add branch of `argLabel` (lib/toolCardPaths.ts): the
 * `text` field is the backlog item's title — the task/prompt being queued.
 * The card reads `backlog_add "Fix the stop_token_ids…"` instead of a bare
 * `backlog_add` with the title hidden behind expand (backlog fef458f1:
 * "backlog_add should have a nice display of the backlog title"). Only the
 * first line is shown (a multi-line prompt collapses to a one-line title) and
 * it is truncated to BACKLOG_TITLE_MAX (80) chars with an ellipsis.
 */
describe("argLabel — backlog_add", () => {
  it("shows a short title quoted", () => {
    expect(argLabel('{"text":"Fix the stop_token_ids"}', "backlog_add")).toBe(
      '"Fix the stop_token_ids"',
    );
  });

  it("uses only the first line of a multi-line title", () => {
    const args = JSON.stringify({ text: "First line of task\nSecond line with detail" });
    expect(argLabel(args, "backlog_add")).toBe('"First line of task"');
  });

  it("truncates a long first line to 79 chars plus an ellipsis", () => {
    const long = "x".repeat(100);
    expect(argLabel(JSON.stringify({ text: long }), "backlog_add")).toBe(`"${"x".repeat(79)}…"`);
  });

  it("does not truncate a first line of exactly BACKLOG_TITLE_MAX (80) chars", () => {
    const exact = "y".repeat(80);
    expect(argLabel(JSON.stringify({ text: exact }), "backlog_add")).toBe(`"${exact}"`);
  });

  it("returns null for blank, empty, missing, or non-string text", () => {
    expect(argLabel('{"text":"   "}', "backlog_add")).toBeNull();
    expect(argLabel('{"text":""}', "backlog_add")).toBeNull();
    expect(argLabel("{}", "backlog_add")).toBeNull();
    expect(argLabel('{"text":123}', "backlog_add")).toBeNull();
  });

  it("returns null for unparseable args (no crash)", () => {
    expect(argLabel("{bad json", "backlog_add")).toBeNull();
  });
});

/**
 * Source-contract for the backlog_add chip's markdown rendering (plan
 * 2026-12): the ToolCard header chip renders the queued item's title
 * through InlineMarkdown — inline md only (bold/code), truncation-safe,
 * never block elements — matching the other read-only backlog surfaces
 * (card, steer bubbles). The InlineMarkdown output
 * contract itself is covered in InlineMarkdown.test.ts.
 */
describe("backlog_add chip — inline markdown wiring", () => {
  it("renders the backlog_add chip text through InlineMarkdown", () => {
    expect(source).toContain('import { InlineMarkdown } from "./InlineMarkdown";');
    expect(source).toContain("<InlineMarkdown text={chip.text} />");
  });

  it("scopes markdown rendering to backlog_add chips only", () => {
    // Other pathless chips (shell purpose, git subcommand, search pattern)
    // stay plain text: the flag is set only on backlog_add's argLabel chip
    // at the push site, and the render site renders markdown only when the
    // chip is flagged — never by tool name.
    expect(source).toContain('md: name === "backlog_add",');
    expect(source).toContain("{chip.md ? <InlineMarkdown text={chip.text} /> : chip.text}");
  });
});
