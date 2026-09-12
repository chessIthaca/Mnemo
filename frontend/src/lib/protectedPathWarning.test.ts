// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The protected-path warning badge (security review 2026-09-09 LOW-2):
 * unit tests on the pure scan helper, plus a ?raw source contract pinning
 * the ApprovalPrompt wiring (shell-scoped, rendered, rationale text).
 */

import { describe, expect, it } from "vitest";
import {
  PROTECTED_PATH_TOKENS,
  protectedPathTokens,
} from "./protectedPathWarning";
import approvalPromptSource from "../components/chat/ApprovalPrompt.tsx?raw";

describe("protectedPathTokens", () => {
  it("flags a command containing .git/ (the acceptance case)", () => {
    expect(protectedPathTokens("rm -rf .git/hooks/pre-commit")).toEqual([
      ".git/",
    ]);
  });

  it("flags a protected cwd appended by the caller (cwd: .git, clean command)", () => {
    expect(
      protectedPathTokens("cp /tmp/evil hooks/pre-commit .git/")
    ).toEqual([".git/"]);
    // The no-cwd composition appends " /" — matches no token.
    expect(protectedPathTokens("cargo test /")).toEqual([]);
  });

  it("normalizes backslash phrasings (Windows-native form)", () => {
    expect(protectedPathTokens("Remove-Item -Recurse .git\\hooks")).toEqual([
      ".git/",
    ]);
    expect(
      protectedPathTokens("cp /tmp/evil hooks/pre-commit .git\\hooks/")
    ).toEqual([".git/"]);
    expect(protectedPathTokens("Remove-Item .CODING\\safety.toml")).toEqual([
      ".coding/",
      "safety.toml",
    ]);
  });

  it("flags each protected token", () => {
    expect(protectedPathTokens("git add .coding/backlog.jsonl")).toEqual([
      ".coding/",
    ]);
    expect(protectedPathTokens("cat .coding/safety.toml")).toEqual([
      ".coding/",
      "safety.toml",
    ]);
  });

  it("matches case-insensitively (Windows paths)", () => {
    expect(protectedPathTokens("RM -RF .GIT/HOOKS")).toEqual([".git/"]);
    expect(protectedPathTokens("Echo hi > .CODING/x.txt")).toEqual([
      ".coding/",
    ]);
  });

  it("does not match .gitignore (the token keeps its trailing slash)", () => {
    expect(protectedPathTokens('echo "*.log" > .gitignore')).toEqual([]);
  });

  it("returns [] for a clean command", () => {
    expect(protectedPathTokens("cargo test")).toEqual([]);
    expect(protectedPathTokens("")).toEqual([]);
  });

  it("lists multiple tokens in canonical order", () => {
    expect(protectedPathTokens("cp .git/config .coding/safety.toml")).toEqual([
      ".coding/",
      "safety.toml",
      ".git/",
    ]);
  });

  it("exposes the token list in canonical order", () => {
    expect([...PROTECTED_PATH_TOKENS]).toEqual([
      ".coding/",
      "safety.toml",
      ".git/",
    ]);
  });
});

describe("ApprovalPrompt badge wiring (source contract)", () => {
  it("imports the scan helper and renders the shell-scoped advisory badge", () => {
    expect(approvalPromptSource).toContain(
      'from "../../lib/protectedPathWarning"'
    );
    // Shell-only scope: file tools enforce the sandbox, shell does not.
    expect(approvalPromptSource).toContain('approval.toolName === "shell"');
    expect(approvalPromptSource).toContain("protectedTokens.length > 0");
    expect(approvalPromptSource).toContain("ShieldAlert");
    expect(approvalPromptSource).toContain("shell writes bypass the file-tool");
    // The scan input joins the command text with the resolved cwd (round-1
    // LOW-1 fix): a bare `.git`/`.coding` cwd must match the slash-bearing
    // tokens.
    expect(approvalPromptSource).toContain(
      '`${args.command} ${args.cwd ?? ""}/`'
    );
  });
});
