// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Chat settings section — regression tests for the move of the chat display
 * toggles ("Show token usage in the activity bar", "Show tool activity in
 * chat") out of Appearance into their own Chat section (user feedback: "where
 * should these go in the config dialog — maybe appearance is the wrong
 * thing"). Static source-contract tests in the style of InflightBar.test.ts
 * (vitest runs in `node` env — no React DOM infra), plus pure-helper tests
 * for the new section's draft serializer.
 */

import { describe, expect, it } from "vitest";
import chatSource from "./ChatSection.tsx?raw";
import appearanceSource from "./AppearanceSection.tsx?raw";
import {
  SETTINGS_NAV,
  isSettingsSectionId,
  serializeChat,
  type ChatDraft,
} from "../types";
import { STEERING_NOTES, type SteeringNoteKey } from "../../../lib/delegationNotes";

describe("Chat settings section placement", () => {
  it("registers 'chat' as a valid Settings section id", () => {
    expect(isSettingsSectionId("chat")).toBe(true);
  });

  it("places Chat right after Appearance in the nav", () => {
    const ids = SETTINGS_NAV.map((n) => n.id);
    expect(ids.indexOf("chat")).toBe(ids.indexOf("appearance") + 1);
  });

  it("chat nav keywords surface the moved toggles in Settings search", () => {
    const keywords = SETTINGS_NAV.find((n) => n.id === "chat")?.keywords ?? [];
    expect(keywords.join(" ")).toContain("token usage");
    expect(keywords.join(" ")).toContain("tool activity");
    expect(keywords.join(" ")).toContain("delegation notes");
  });

  it("appearance nav no longer claims the moved toggles", () => {
    const keywords =
      SETTINGS_NAV.find((n) => n.id === "appearance")?.keywords ?? [];
    expect(keywords.join(" ")).not.toContain("token usage");
  });
});

describe("ChatSection owns the chat display toggles", () => {
  it("renders all the chat display checkboxes", () => {
    expect(chatSource).toContain("Show token usage in the activity bar");
    expect(chatSource).toContain("Show images from image tools in chat");
    expect(chatSource).toContain("Show tool activity in chat");
    expect(chatSource).toContain("Show knowledge activity in chat (graph, memory, auto-recall)");
    // Per-kind steering-note toggles (registry-driven sub-block).
    expect(chatSource).toContain("Steering notes in tool results");
    expect(chatSource).toContain("{STEERING_NOTES.map((def) => (");
    expect(chatSource).toContain("checked={draft.steeringNotes[def.key]}");
    // The four chat-readability affordances (plan afa81f0a).
    expect(chatSource).toContain("Thread line along activity cards");
    expect(chatSource).toContain("Cap prose width (~100 columns)");
    expect(chatSource).toContain("Alternating turn background");
    expect(chatSource).toContain("Timestamps on hover");
  });

  it("persists all the toggles via saveSettings (config.toml [ui] keys)", () => {
    expect(chatSource).toContain("show_token_usage: draft.showTokenUsage");
    expect(chatSource).toContain("show_tool_images: draft.showToolImages");
    expect(chatSource).toContain("show_tool_activity: draft.showToolActivity");
    expect(chatSource).toContain("show_knowledge_activity: draft.showKnowledgeActivity");
    expect(chatSource).toContain("steering_notes: draft.steeringNotes");
    expect(chatSource).toContain("chat_thread_line: draft.chatThreadLine");
    expect(chatSource).toContain("chat_prose_cap: draft.chatProseCap");
    expect(chatSource).toContain("chat_turn_tint: draft.chatTurnTint");
    expect(chatSource).toContain("chat_hover_timestamps: draft.chatHoverTimestamps");
  });

  it("commits all the toggles to the store on save", () => {
    expect(chatSource).toContain("setShowTokenUsage(draft.showTokenUsage)");
    expect(chatSource).toContain("setShowToolImages(draft.showToolImages)");
    expect(chatSource).toContain("setShowToolActivity(draft.showToolActivity)");
    expect(chatSource).toContain("setShowKnowledgeActivity(draft.showKnowledgeActivity)");
    expect(chatSource).toContain("s.setHiddenSteeringNotes(");
    expect(chatSource).toContain("setChatThreadLine(draft.chatThreadLine)");
    expect(chatSource).toContain("setChatProseCap(draft.chatProseCap)");
    expect(chatSource).toContain("setChatTurnTint(draft.chatTurnTint)");
    expect(chatSource).toContain("setChatHoverTimestamps(draft.chatHoverTimestamps)");
  });

  it("AppearanceSection no longer contains the chat toggles (moved out)", () => {
    expect(appearanceSource).not.toContain("showTokenUsage");
    expect(appearanceSource).not.toContain("showToolActivity");
    expect(appearanceSource).not.toContain("show_token_usage");
    expect(appearanceSource).not.toContain("show_tool_activity");
  });
});

describe("serializeChat", () => {
  /** Every kind visible — the draft's positive form of the store's list. */
  const allVisible = Object.fromEntries(
    STEERING_NOTES.map((def) => [def.key, true]),
  ) as Record<SteeringNoteKey, boolean>;
  const base: ChatDraft = {
    showTokenUsage: true,
    showToolImages: true,
    showToolActivity: true,
    showKnowledgeActivity: true,
    steeringNotes: allVisible,
    chatThreadLine: true,
    chatProseCap: true,
    chatTurnTint: true,
    chatHoverTimestamps: true,
  };

  it("serializes identical drafts identically (clean state → not dirty)", () => {
    expect(serializeChat(base)).toBe(serializeChat({ ...base }));
  });

  it("distinguishes drafts that differ in any toggle (dirty detection)", () => {
    expect(serializeChat(base)).not.toBe(
      serializeChat({ ...base, showTokenUsage: false }),
    );
    expect(serializeChat(base)).not.toBe(
      serializeChat({ ...base, showToolImages: false }),
    );
    expect(serializeChat(base)).not.toBe(
      serializeChat({ ...base, showToolActivity: false }),
    );
    expect(serializeChat(base)).not.toBe(
      serializeChat({ ...base, steeringNotes: { ...allVisible, literal_tip: false } }),
    );
    expect(serializeChat(base)).not.toBe(
      serializeChat({ ...base, chatThreadLine: false }),
    );
    expect(serializeChat(base)).not.toBe(
      serializeChat({ ...base, chatProseCap: false }),
    );
    expect(serializeChat(base)).not.toBe(
      serializeChat({ ...base, chatTurnTint: false }),
    );
    expect(serializeChat(base)).not.toBe(
      serializeChat({ ...base, chatHoverTimestamps: false }),
    );
  });
});
