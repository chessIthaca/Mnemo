// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * SOURCE-CONTRACT regression guard for the agent-event listener-registration
 * failure path (quality review Q3, 2026-09-08). `ensureListenerStarted`
 * registers the module-level Tauri listener with a void-discarded promise:
 * a rejection (denied core:event permission, IPC failure) leaves the window
 * with NO agent-event listener — every Rust->TS event is then dropped with
 * no error on either side, which looks exactly like "the app works but the
 * UI never updates" (the live 2026-09-08 blackout). The registration only
 * runs inside a real webview, so the contract is asserted against the
 * source text — the same pattern as themeContrast.test.ts /
 * toolCardPaths.test.ts. FAILS LOUDLY on a missing/renamed function (never
 * silently passes on a stale source shape).
 */

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const src = readFileSync(new URL("./useAgentEvents.ts", import.meta.url), "utf8");

/**
 * The `ensureListenerStarted` function body, bounded by the next top-level
 * doc comment — the sibling listener starters follow it.
 */
function ensureListenerStartedSource(): string {
  const start = src.indexOf("function ensureListenerStarted");
  if (start === -1) {
    throw new Error("ensureListenerStarted not found in useAgentEvents.ts");
  }
  const end = src.indexOf("\n/**", start);
  if (end === -1) {
    throw new Error("ensureListenerStarted is not followed by a doc comment");
  }
  return src.slice(start, end);
}

describe("ensureListenerStarted registration failure surfacing (source contract)", () => {
  const fn = ensureListenerStartedSource();

  it("registers via onAgentEvent and chains .then + .catch on the promise", () => {
    expect(fn).toContain("onAgentEvent(");
    expect(fn).toContain(".then(");
    expect(fn).toContain(".catch(");
  });

  it("the .catch reports via console.error AND uiDiag with a FATAL marker", () => {
    const catchBody = fn.slice(fn.indexOf(".catch("));
    expect(catchBody).toContain("console.error");
    expect(catchBody).toContain("uiDiag");
    expect(catchBody).toContain("FATAL");
  });

  it("the .then reports the registered channel via uiDiag", () => {
    const thenBody = fn.slice(fn.indexOf(".then("), fn.indexOf(".catch("));
    expect(thenBody).toContain("uiDiag");
  });

  it("the registration call is chained, not void-discarded", () => {
    expect(fn).not.toContain("void onAgentEvent(");
    expect(fn).toMatch(/onAgentEvent\([\s\S]*?\)\s*\.then\(/);
  });
});
