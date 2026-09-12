// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Unit tests for the file-type / language helpers the File viewer uses to
 * decide how to render a clicked file (`frontend/src/lib/language.ts`).
 */

import { describe, expect, it } from "vitest";

import {
  isMarkdownPath,
  isTextPath,
  languageForPath,
  wrapCodeFence,
} from "./language";

describe("languageForPath", () => {
  it("maps markdown to the markdown language", () => {
    expect(languageForPath("README.md")).toBe("markdown");
    expect(languageForPath("docs/guide.markdown")).toBe("markdown");
    expect(languageForPath("a/b/page.mdx")).toBe("markdown");
  });

  it("maps common code extensions", () => {
    expect(languageForPath("src/main.rs")).toBe("rust");
    expect(languageForPath("src/App.tsx")).toBe("typescript");
    expect(languageForPath("lib/util.js")).toBe("javascript");
    expect(languageForPath("config.toml")).toBe("ini");
    expect(languageForPath("data.json")).toBe("json");
    expect(languageForPath("styles.css")).toBe("css");
    expect(languageForPath("script.py")).toBe("python");
  });

  it("is case-insensitive and handles Windows backslashes", () => {
    expect(languageForPath("SRC\\MAIN.RS")).toBe("rust");
    expect(languageForPath("a\\b\\ReadMe.MD")).toBe("markdown");
  });

  it("returns null for unknown extensions and dotfiles", () => {
    expect(languageForPath("file.unknownext")).toBeNull();
    expect(languageForPath(".gitignore")).toBeNull();
    expect(languageForPath("Makefile")).toBeNull();
  });
});

describe("isTextPath", () => {
  it("treats code, text, and extensionless files as text", () => {
    expect(isTextPath("src/main.rs")).toBe(true);
    expect(isTextPath("README.md")).toBe(true);
    expect(isTextPath("Makefile")).toBe(true);
    expect(isTextPath(".gitignore")).toBe(true);
  });

  it("treats images, archives, and binaries as not text", () => {
    expect(isTextPath("logo.png")).toBe(false);
    expect(isTextPath("archive.zip")).toBe(false);
    expect(isTextPath("app.exe")).toBe(false);
    expect(isTextPath("db.sqlite")).toBe(false);
  });
});

describe("isMarkdownPath", () => {
  it("is true only for markdown", () => {
    expect(isMarkdownPath("README.md")).toBe(true);
    expect(isMarkdownPath("src/main.rs")).toBe(false);
  });
});

describe("wrapCodeFence", () => {
  it("wraps content in a fenced block with the language", () => {
    expect(wrapCodeFence("fn main() {}", "rust")).toBe("```rust\nfn main() {}\n```");
  });

  it("uses an empty info string when language is null", () => {
    expect(wrapCodeFence("plain", null)).toBe("```\nplain\n```");
  });

  it("lengthens the fence so embedded triple-backticks can't break out", () => {
    const content = "here is a fence:\n```\ncode\n```\ndone";
    const out = wrapCodeFence(content, "markdown");
    // The outer fence must be longer than the embedded ``` (3) — so 4.
    expect(out.startsWith("````markdown\n")).toBe(true);
    expect(out.endsWith("\n````")).toBe(true);
  });
});
