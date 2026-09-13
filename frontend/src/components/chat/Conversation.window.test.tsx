// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Conversation transcript windowing (mem-perf): the render window + expander
 * pinned via renderToStaticMarkup (the BacklogView pattern — vitest runs in
 * node, no DOM; Conversation takes its state as a prop and useAgentStore
 * reads initial state under SSR, so no store mocking). windowEntries is
 * unit-tested directly.
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Conversation, TRANSCRIPT_WINDOW, windowEntries } from "./Conversation";
import { emptyAgentState } from "../../hooks/agentState";
import type { AgentState } from "../../hooks/agentState";
import type { TranscriptEntry } from "../../lib/types";
import source from "./Conversation.tsx?raw";

/** A transcript of alternating user/assistant entries with unique markers
 *  (msg-000…msg-(n-1)) and explicit numeric entryIds (the React-key stamp). */
function fakeTranscript(n: number): TranscriptEntry[] {
  const entries: TranscriptEntry[] = [];
  for (let i = 0; i < n; i++) {
    const marker = `msg-${String(i).padStart(3, "0")}`;
    entries.push(
      i % 2 === 0
        ? { kind: "user", text: marker, entryId: i }
        : { kind: "assistant", text: marker, entryId: i },
    );
  }
  return entries;
}

function stateWith(
  transcript: TranscriptEntry[],
  overrides: Partial<AgentState> = {},
): AgentState {
  return { ...emptyAgentState(), transcript, ...overrides };
}

function renderConversation(state: AgentState): string {
  // The activity toggles are store reads (initial values under SSR — the
  // default showToolActivity=true is fine: user/assistant entries pass the
  // filter regardless).
  return renderToStaticMarkup(<Conversation state={state} />);
}

describe("Conversation transcript windowing (render)", () => {
  it("renders only the last TRANSCRIPT_WINDOW entries of a long transcript", () => {
    const html = renderConversation(stateWith(fakeTranscript(250)));
    // The first 50 are windowed away…
    expect(html).not.toContain("msg-049");
    // …the last 200 all render.
    for (let i = 50; i < 250; i++) {
      expect(html).toContain(`msg-${String(i).padStart(3, "0")}`);
    }
  });

  it("shows the expander with the hidden count when entries are windowed away", () => {
    const html = renderConversation(stateWith(fakeTranscript(250)));
    expect(html).toContain("Show earlier messages (50 hidden)");
  });

  it("renders everything with no expander at or under the window", () => {
    const html = renderConversation(stateWith(fakeTranscript(TRANSCRIPT_WINDOW)));
    expect(html).toContain("msg-000");
    expect(html).toContain(`msg-${String(TRANSCRIPT_WINDOW - 1).padStart(3, "0")}`);
    expect(html).not.toContain("Show earlier messages");
  });

  it("never windows away the live stream", () => {
    const html = renderConversation(
      stateWith(fakeTranscript(250), { streamingText: "LIVE-STREAM-MARKER" }),
    );
    expect(html).toContain("LIVE-STREAM-MARKER");
  });
});

describe("windowEntries (pure helper)", () => {
  const entries = [1, 2, 3, 4, 5];

  it("returns the same array with hidden 0 under the window", () => {
    const r = windowEntries(entries, 10);
    expect(r.windowed).toBe(entries);
    expect(r.hidden).toBe(0);
  });

  it("returns the tail slice + hidden count over the window", () => {
    const r = windowEntries(entries, 3);
    expect(r.windowed).toEqual([3, 4, 5]);
    expect(r.hidden).toBe(2);
  });

  it("a grown window covers earlier entries", () => {
    const r = windowEntries(entries, 5);
    expect(r.windowed).toBe(entries);
    expect(r.hidden).toBe(0);
  });

  it("handles the empty transcript", () => {
    const r = windowEntries([], 200);
    expect(r.windowed).toEqual([]);
    expect(r.hidden).toBe(0);
  });
});

describe("Conversation windowing (source contracts)", () => {
  it("pins the default window at 200 entries", () => {
    expect(source).toContain("export const TRANSCRIPT_WINDOW = 200;");
  });

  it("windows the post-filter visible list inside the memo", () => {
    expect(source).toContain("windowEntries(visible, windowSize)");
  });

  it("the expander grows the window by TRANSCRIPT_WINDOW per click", () => {
    expect(source).toContain("setWindowSize((w) => w + TRANSCRIPT_WINDOW)");
  });

  it("the memo deps include windowSize (growing re-windows)", () => {
    expect(source).toContain(
      "[state.transcript, showToolActivity, showKnowledgeActivity, windowSize]",
    );
  });

  it("the auto-scroll effect is untouched (deps + endRef after the turns list)", () => {
    expect(source).toContain(
      "[state.transcript, state.streamingText, state.pendingApproval]",
    );
    // endRef renders after the turns map — streaming appends + auto-scrolls.
    const turnsMap = source.indexOf("{turns.map((turn, ti) => (");
    const endRef = source.indexOf('<div ref={endRef} />');
    expect(turnsMap).toBeGreaterThan(-1);
    expect(endRef).toBeGreaterThan(turnsMap);
  });

  it("adds no virtualization dependency", () => {
    expect(source).not.toContain("react-window");
    expect(source).not.toContain("virtuoso");
  });
});
