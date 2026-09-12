// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * errMsg (frontend/src/lib/tauri.ts) — the unknown-rejection → string
 * extraction every error surface uses. Pinned here for the Backlog tab's
 * add-rejection surface (backlog 45a4eb88): the backend rejects shape-less
 * text with a structured `{ kind, message }` IpcError, and a bare
 * `String(e)` would render the DTO as "[object Object]".
 */

import { describe, expect, it } from "vitest";
import { errMsg } from "./tauri";

describe("errMsg", () => {
  it("passes a bare string through", () => {
    expect(errMsg("boom")).toBe("boom");
  });

  it("extracts an Error's message", () => {
    expect(errMsg(new Error("disk full"))).toBe("disk full");
  });

  it("extracts the message from a structured IpcError DTO ({ kind, message })", () => {
    expect(
      errMsg({ kind: "error", message: "backlog item needs a headline AND a body" }),
    ).toBe("backlog item needs a headline AND a body");
  });

  it("falls back to String(e) when there is no string message", () => {
    expect(errMsg(42)).toBe("42");
    expect(errMsg(null)).toBe("null");
    expect(errMsg({ message: 42 })).toBe("[object Object]");
  });
});
