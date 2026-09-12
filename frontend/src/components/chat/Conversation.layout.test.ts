// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Transcript layout contracts (user request 2027-01-04): the agent output
 * window fills the available width (no centered max-w column, no content
 * caps) and entries sit on a readability indent ladder — prompts (user +
 * steer) flush LEFT, agent prose at pl-[1em], tool/memory/activity at
 * pl-[2em] — with turn-grouping spacing before prompts. Static
 * source-contract tests in the style of Conversation.test.ts (vitest runs
 * in `node` env — no React DOM infra).
 */
import { describe, expect, it } from "vitest";
import conversationSource from "./Conversation.tsx?raw";
import messageSource from "./Message.tsx?raw";

describe("transcript width", () => {
  it("fills the available width — no centered max-w column", () => {
    expect(conversationSource).not.toContain("max-w-4xl");
    expect(conversationSource).toContain("space-y-[0.5em]");
  });

  it("drops the message content caps", () => {
    expect(messageSource).not.toContain("max-w-3xl");
  });
});

describe("prompt alignment (level 0 — flush left)", () => {
  it("left-aligns user messages and steers", () => {
    expect(messageSource).toContain("items-start");
    expect(messageSource).toContain("justify-start");
    // Absence scoped to the OLD prompt wrapper classes: Message.tsx also
    // hosts ToolCard/CallDetail/memory/vision cards, where a future
    // items-end/justify-end would be legitimate and must not fail this
    // prompt-alignment pin (review F1).
    expect(messageSource).not.toContain("flex flex-col items-end");
    expect(messageSource).not.toContain("flex justify-end");
  });

  it("flips the bubble corner cue to the left", () => {
    expect(messageSource).toContain("rounded-bl-sm");
    expect(messageSource).not.toContain("rounded-br-sm");
  });

  it("groups turns with extra top spacing before prompts", () => {
    expect(messageSource).toContain("pt-[0.75em]");
  });
});

describe("indent ladder", () => {
  it("indents agent prose one step (assistant/error/qa + interactive prompts)", () => {
    expect(messageSource).toContain("pl-[1em]");
    expect(conversationSource).toContain("pl-[1em]");
  });

  it("indents tool/memory/vision/skill activity two steps (the run wrapper owns it)", () => {
    // Message renders the activity cards bare; Conversation's activity-run
    // wrapper provides the level-2 indent (ml-[2em] baseline).
    expect(messageSource).not.toContain("pl-[2em]");
    expect(conversationSource).toContain('"ml-[2em]"');
  });
});

describe("chat readability affordances (plan afa81f0a)", () => {
  it("draws the thread line along activity runs (toggleable)", () => {
    expect(conversationSource).toContain(
      "ml-[1.5em] border-l border-slate-700/60 pl-[0.5em]",
    );
    expect(conversationSource).toContain("chatThreadLine");
  });

  it("alternates a faint background per turn (toggleable)", () => {
    expect(conversationSource).toContain("bg-bg-tertiary/30");
    expect(conversationSource).toContain("chatTurnTint");
  });

  it("caps prose at ~100 columns, code blocks stay full width (toggleable)", () => {
    expect(messageSource).toContain("prose-p:max-w-[100ch]");
    expect(messageSource).toContain("prose-li:max-w-[100ch]");
    expect(messageSource).toContain("prose-blockquote:max-w-[100ch]");
    expect(messageSource).toContain("chatProseCap");
  });

  it("wraps entries with a hover-timestamp title (toggleable)", () => {
    expect(conversationSource).toContain("chatHoverTimestamps");
    expect(conversationSource).toContain("fmtTs");
  });
});
