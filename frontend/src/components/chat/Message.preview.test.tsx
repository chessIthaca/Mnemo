// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Live shell-output preview render contract (backlog 6f25fb7e, plan
 * 90c15456): the preview block in a running shell card must occupy a FIXED
 * height — reserved from the first paint, BEFORE any output arrives — so
 * streaming chunks can never change the card's height or jump the chat
 * column. The toggle-off path lives in Message.preview.off.test.tsx (it
 * needs the store's INITIAL state seeded false — see there).
 *
 * Node-env harness (the repo's established pattern — RightPanel.width
 * .test.tsx): renderToStaticMarkup over the real Message component with a
 * tool entry; the store reads fall to the initial state (the toggle
 * defaults ON in a node env with no window/localStorage).
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Message } from "./Message";
import type { ToolInvocation, TranscriptEntry } from "../../lib/types";

/** A running shell call (no result yet); liveOutput optional. */
function shellCall(liveOutput?: string): ToolInvocation {
  return {
    id: "c1",
    index: 0,
    args: JSON.stringify({ command: "cargo test" }),
    result: null,
    ...(liveOutput !== undefined ? { liveOutput } : {}),
  };
}

function toolEntry(name: string, calls: ToolInvocation[]): TranscriptEntry {
  return { kind: "tool", name, calls };
}

describe("live shell-output preview — fixed height, reserved (backlog 6f25fb7e)", () => {
  it("renders the block at a FIXED height from the first paint, before any output", () => {
    // A running shell call with NO liveOutput yet: the block must already
    // be there at its full fixed height (reserved), so the first chunk
    // cannot change the card's height.
    const markup = renderToStaticMarkup(
      <Message entry={toolEntry("shell", [shellCall()])} />,
    );
    expect(markup).toContain("h-[10em]");
    expect(markup).toContain('aria-live="polite"');
    expect(markup).not.toContain("max-h-[12em]");
  });

  it("renders the streamed tail inside the fixed block", () => {
    const markup = renderToStaticMarkup(
      <Message entry={toolEntry("shell", [shellCall("Compiling mnemo v0.1.0\n")])} />,
    );
    expect(markup).toContain("h-[10em]");
    expect(markup).toContain("Compiling mnemo v0.1.0");
  });

  it("renders no preview block for a running NON-shell call", () => {
    // Only shell emits ToolOutputDelta (the shell.rs sink) — no reservation
    // for any other tool, even with a stray liveOutput.
    const markup = renderToStaticMarkup(
      <Message entry={toolEntry("search", [shellCall("partial output")])} />,
    );
    expect(markup).not.toContain('aria-live="polite"');
    expect(markup).not.toContain("h-[10em]");
  });

  it("renders no preview block once the call has finished", () => {
    // The result replaces the tail — a finished card never shows a stale
    // preview.
    const markup = renderToStaticMarkup(
      <Message
        entry={toolEntry("shell", [
          {
            id: "c1",
            index: 0,
            args: JSON.stringify({ command: "cargo test" }),
            result: { success: true, output: "done" },
          },
        ])}
      />,
    );
    expect(markup).not.toContain('aria-live="polite"');
    expect(markup).not.toContain("h-[10em]");
  });
});

describe("tool-card timing — duration + wall-clock next to the check (user request 2027-01-24)", () => {
  /** A completed call with explicit timing stamps (deterministic duration). */
  function doneCall(startedAt: number, endedAt: number): ToolInvocation {
    return {
      id: "c1",
      index: 0,
      args: JSON.stringify({ command: "cargo test" }),
      result: { success: true, output: "done" },
      startedAt,
      endedAt,
    };
  }

  it("renders the duration and the wall-clock start next to the check", () => {
    // A minute ago (still today → time-only), exactly 2.3s long.
    const startedAt = Date.now() - 60_000;
    const markup = renderToStaticMarkup(
      <Message entry={toolEntry("shell", [doneCall(startedAt, startedAt + 2300)])} />,
    );
    expect(markup).toContain("✓");
    expect(markup).toContain(" · 2.3s · ");
    expect(markup).toContain(new Date(startedAt).toLocaleTimeString());
  });

  it("renders no timing for legacy invocations without stamps", () => {
    // Old persisted transcripts have no startedAt/endedAt — the card must
    // render exactly as before (check only, no timing segments).
    const markup = renderToStaticMarkup(
      <Message
        entry={toolEntry("shell", [
          {
            id: "c1",
            index: 0,
            args: JSON.stringify({ command: "cargo test" }),
            result: { success: true, output: "done" },
          },
        ])}
      />,
    );
    expect(markup).toContain("✓");
    expect(markup).not.toContain(" · ");
  });

  it("renders no timing while the call is running", () => {
    const markup = renderToStaticMarkup(
      <Message entry={toolEntry("shell", [shellCall()])} />,
    );
    expect(markup).toContain("running");
    expect(markup).not.toContain(" · ");
  });
});
