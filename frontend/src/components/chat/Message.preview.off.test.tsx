// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The toggle-OFF half of the live shell-output preview contract (backlog
 * 6f25fb7e, plan 90c15456): with mh.showShellPreview="false" persisted, no
 * preview block renders at all — not even the reserved empty one.
 *
 * Node-env harness: renderToStaticMarkup makes useSyncExternalStore read
 * the store's INITIAL state, so the toggle is seeded through a fake
 * localStorage on a `window` stub installed via vi.hoisted BEFORE the
 * store module initializes (the RightPanel.width.test.tsx pattern). The
 * toggle-ON cases live in Message.preview.test.tsx.
 */

import { describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  (globalThis as { window?: unknown }).window = {
    localStorage: {
      getItem: (key: string) =>
        key === "mh.showShellPreview" ? "false" : null,
      setItem: () => {},
    },
    matchMedia: () => ({ matches: false }),
  };
});

import { renderToStaticMarkup } from "react-dom/server";
import { Message } from "./Message";
import type { ToolInvocation, TranscriptEntry } from "../../lib/types";

describe("live shell-output preview — toggle off (backlog 6f25fb7e)", () => {
  it("renders no preview block at all, not even the reserved one", () => {
    const call: ToolInvocation = {
      id: "c1",
      index: 0,
      args: JSON.stringify({ command: "cargo test" }),
      result: null,
      liveOutput: "Compiling mnemo v0.1.0\n",
    };
    const entry: TranscriptEntry = { kind: "tool", name: "shell", calls: [call] };
    const markup = renderToStaticMarkup(<Message entry={entry} />);
    expect(markup).not.toContain('aria-live="polite"');
    expect(markup).not.toContain("h-[10em]");
    // The streamed text must not leak into the card either.
    expect(markup).not.toContain("Compiling mnemo v0.1.0");
  });
});
